use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::config::{IpPreference, ServerConfig};
use crate::vendor::nntp::{
    Credentials, Downloads, Endpoint, Family, Pool, PoolConfig, Provider, TlsMode, Usage,
};
use crate::{Error, day};

const TIMEOUT: Duration = Duration::from_secs(30);
const IDLE: Duration = Duration::from_secs(60);
const DAY: u64 = 24 * 60 * 60;

pub struct Accounts {
    usage: Mutex<HashMap<String, Arc<Usage>>>,
}

pub struct Shape {
    pub pipeline_depth: usize,
    pub min_connections: usize,
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
        let downloads = Downloads::new((total * shape.pipeline_depth).max(1));
        usable
            .into_iter()
            .map(|server| {
                let fail = |source| Error::Server {
                    server: server.id.clone(),
                    source,
                };
                let endpoint = Endpoint::new(
                    server.host.clone(),
                    server.port,
                    tls_mode(server),
                    credentials(server),
                    TIMEOUT,
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
                        min_connections: shape.min_connections.min(server.connections),
                    },
                    Arc::clone(&downloads),
                )
                .map_err(fail)?;
                let pool = match server.retention_days {
                    Some(days) => pool.with_retention(Duration::from_secs(u64::from(days) * DAY)),
                    None => pool,
                };
                Ok(Provider {
                    pool,
                    key: server.id.clone(),
                    name: server.name.clone(),
                    priority: server.priority,
                    backbone: server.backbone.clone(),
                })
            })
            .collect()
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
