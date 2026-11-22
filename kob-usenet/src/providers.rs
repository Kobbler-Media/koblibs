use std::collections::HashMap;
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
    pools: Mutex<HashMap<String, Built>>,
    downloads: Arc<Downloads>,
}

struct Built {
    server: ServerConfig,
    shape: (usize, usize, Duration),
    pool: Pool,
    account: Arc<Semaphore>,
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
            pools: Mutex::default(),
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
        let total: usize = usable.iter().map(|s| s.connections).sum();
        self.downloads.resize((total * shape.pipeline_depth).max(1));
        let first = usable.iter().map(|s| s.priority).min();
        let mut pools = self.pools.lock().unwrap_or_else(PoisonError::into_inner);
        let providers = usable
            .iter()
            .map(|&server| {
                let warm = if Some(server.priority) == first {
                    shape.min_connections.min(server.connections)
                } else {
                    0
                };
                let pool = self.pool(&mut pools, server, shape, warm)?;
                Ok(Provider {
                    pool,
                    key: server.id.clone(),
                    name: server.name.clone(),
                    priority: server.priority,
                    backbone: server.backbone.clone(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        pools.retain(|id, _| usable.iter().any(|s| &s.id == id));
        Ok(providers)
    }

    fn pool(
        &self,
        pools: &mut HashMap<String, Built>,
        server: &ServerConfig,
        shape: &Shape,
        warm: usize,
    ) -> Result<Pool, Error> {
        let wanted = (shape.pipeline_depth, warm, shape.timeout);
        let account = match pools.get(&server.id) {
            Some(built) if built.shape == wanted && same_connection(&built.server, server) => {
                return Ok(built.pool.clone());
            }
            Some(built) => {
                resize(&built.account, built.server.connections, server.connections);
                Arc::clone(&built.account)
            }
            None => Arc::new(Semaphore::new(server.connections)),
        };
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
        .with_account(Arc::clone(&account));
        let pool = match server.retention_days {
            Some(days) => pool.with_retention(Duration::from_secs(u64::from(days) * DAY)),
            None => pool,
        };
        pools.insert(
            server.id.clone(),
            Built {
                server: server.clone(),
                shape: wanted,
                pool: pool.clone(),
                account,
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

fn resize(account: &Arc<Semaphore>, from: usize, to: usize) {
    if to > from {
        account.add_permits(to - from);
    } else if from > to {
        let account = Arc::clone(account);
        let drop = u32::try_from(from - to).unwrap_or(u32::MAX);
        tokio::spawn(async move {
            if let Ok(permits) = account.acquire_many_owned(drop).await {
                permits.forget();
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
