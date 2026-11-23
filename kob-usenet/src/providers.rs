use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::config::{IpPreference, ServerConfig};
use crate::vendor::nntp::{
    Credentials, Downloads, Endpoint, Family, Pool, PoolConfig, Provider, TlsMode, Usage,
};
use crate::{Error, day};

const IDLE: Duration = Duration::from_secs(60);
const DAY: u64 = 24 * 60 * 60;

pub struct Accounts {
    usage: Mutex<HashMap<String, Arc<Usage>>>,
    registry: Mutex<Registry>,
    downloads: Arc<Downloads>,
}

#[derive(Default)]
struct Registry {
    pools: HashMap<String, Built>,
    accounts: HashMap<String, Account>,
}

struct Built {
    server: ServerConfig,
    shape: (usize, usize, Duration),
    pool: Pool,
}

#[derive(Clone)]
struct Account {
    connections: usize,
    permits: Arc<Semaphore>,
    owed: Arc<AtomicUsize>,
}

impl Account {
    fn new(connections: usize) -> Self {
        Self {
            connections,
            permits: Arc::new(Semaphore::new(connections)),
            owed: Arc::default(),
        }
    }

    // Pools, their open connections and a pending reclaim all hold the semaphore.
    fn in_use(&self) -> bool {
        Arc::strong_count(&self.permits) > 1
    }

    fn set_connections(&mut self, connections: usize) {
        resize(self, self.connections, connections);
        self.connections = connections;
    }
}

pub struct Shape {
    pub pipeline_depth: usize,
    pub min_connections: usize,
    pub timeout: Duration,
}

impl Accounts {
    pub fn new(saved: HashMap<String, u64>) -> Self {
        let usage = saved
            .into_iter()
            .map(|(id, used)| {
                let usage = Usage::default();
                usage.set_used(used);
                (id, Arc::new(usage))
            })
            .collect();
        Self {
            usage: Mutex::new(usage),
            registry: Mutex::default(),
            downloads: Downloads::new(1),
        }
    }

    fn usage(&self, server: &ServerConfig) -> Arc<Usage> {
        let mut usage = self.usage.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = Arc::clone(usage.entry(server.id.clone()).or_default());
        entry.set_cap(server.quota_bytes);
        entry
    }

    pub fn used(&self) -> HashMap<String, u64> {
        self.usage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(id, usage)| (id.clone(), usage.used()))
            .collect()
    }

    pub fn usable<'a>(&self, servers: &'a [ServerConfig]) -> Vec<&'a ServerConfig> {
        let today = day::today();
        servers
            .iter()
            .filter(|s| !s.expired(today) && !self.usage(s).exhausted())
            .collect()
    }

    pub fn build(&self, servers: &[ServerConfig], shape: &Shape) -> Result<Vec<Provider>, Error> {
        let usable = self.usable(servers);
        for server in &usable {
            validate(server)?;
        }
        let first = usable.iter().map(|s| s.priority).min();
        let total: usize = usable
            .iter()
            .filter(|s| Some(s.priority) == first)
            .map(|s| s.connections)
            .sum();
        self.downloads.resize((total * shape.pipeline_depth).max(1));
        let mut registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
        let providers = usable
            .iter()
            .map(|&server| {
                let warm = if Some(server.priority) == first {
                    shape.min_connections.min(server.connections)
                } else {
                    0
                };
                let pool = self.pool(&mut registry, server, shape, warm)?;
                Ok(Provider {
                    pool,
                    key: server.id.clone(),
                    name: server.name.clone(),
                    priority: server.priority,
                    backbone: server.backbone.clone(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let listed = |id: &String| usable.iter().any(|s| &s.id == id);
        registry.pools.retain(|id, _| listed(id));
        registry
            .accounts
            .retain(|id, account| listed(id) || account.in_use());
        Ok(providers)
    }

    fn pool(
        &self,
        registry: &mut Registry,
        server: &ServerConfig,
        shape: &Shape,
        warm: usize,
    ) -> Result<Pool, Error> {
        let wanted = (shape.pipeline_depth, warm, shape.timeout);
        if let Some(built) = registry.pools.get(&server.id)
            && built.shape == wanted
            && same_connection(&built.server, server)
        {
            return Ok(built.pool.clone());
        }
        let account = registry
            .accounts
            .entry(server.id.clone())
            .and_modify(|account| account.set_connections(server.connections))
            .or_insert_with(|| Account::new(server.connections));
        let permits = Arc::clone(&account.permits);
        let fail = |source| Error::Server {
            server: server.id.clone(),
            source,
        };
        let endpoint = Endpoint::new(
            server.host.clone(),
            server.port,
            tls_mode(server),
            credentials(server),
            shape.timeout,
        )
        .map_err(fail)?
        .with_family(family(server.ip_preference))
        .with_usage(self.usage(server));
        let pool = Pool::new(
            endpoint,
            PoolConfig {
                max_connections: server.connections,
                pipeline_depth: shape.pipeline_depth,
                idle_timeout: IDLE,
                min_connections: warm,
            },
            Arc::clone(&self.downloads),
        )
        .map_err(fail)?
        .with_account(permits);
        let pool = match server.retention_days {
            Some(days) => pool.with_retention(Duration::from_secs(u64::from(days) * DAY)),
            None => pool,
        };
        registry.pools.insert(
            server.id.clone(),
            Built {
                server: server.clone(),
                shape: wanted,
                pool: pool.clone(),
            },
        );
        Ok(pool)
    }
}

fn same_connection(old: &ServerConfig, new: &ServerConfig) -> bool {
    let normalized = |s: &ServerConfig| ServerConfig {
        name: String::new(),
        priority: 0,
        backbone: None,
        expires_on: None,
        quota_bytes: None,
        ..s.clone()
    };
    normalized(old) == normalized(new)
}

fn validate(server: &ServerConfig) -> Result<(), Error> {
    let username = &server.username;
    let bad_username = !username.is_empty() && !username.bytes().all(|b| b.is_ascii_graphic());
    let bad_password = server.password.bytes().any(|b| b == b'\r' || b == b'\n');
    if bad_username || bad_password {
        return Err(Error::Server {
            server: server.id.clone(),
            source: crate::vendor::nntp::Error::InvalidArgument("credentials"),
        });
    }
    Ok(())
}

fn resize(account: &Account, from: usize, to: usize) {
    if to > from {
        let raise = to - from;
        let cancelled = account
            .owed
            .try_update(Ordering::AcqRel, Ordering::Acquire, |owed| {
                Some(owed - owed.min(raise))
            })
            .map_or(0, |owed| owed.min(raise));
        account.permits.add_permits(raise - cancelled);
    } else if from > to && account.owed.fetch_add(from - to, Ordering::AcqRel) == 0 {
        let account = account.clone();
        tokio::spawn(async move {
            while account.owed.load(Ordering::Acquire) > 0 {
                let Ok(permit) = account.permits.acquire().await else {
                    return;
                };
                let paid = account
                    .owed
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |owed| {
                        owed.checked_sub(1)
                    });
                if paid.is_ok() {
                    permit.forget();
                }
            }
        });
    }
}

const fn family(preference: IpPreference) -> Family {
    match preference {
        IpPreference::Any => Family::Any,
        IpPreference::V4 => Family::PreferV4,
        IpPreference::V6 => Family::PreferV6,
    }
}

const fn tls_mode(server: &ServerConfig) -> TlsMode {
    match (server.tls, server.verify_cert) {
        (false, _) => TlsMode::Plain,
        (true, true) => TlsMode::Verified,
        (true, false) => TlsMode::Unverified,
    }
}

fn credentials(server: &ServerConfig) -> Option<Credentials> {
    (!server.username.is_empty()).then(|| Credentials {
        username: server.username.clone(),
        password: server.password.clone(),
    })
}
