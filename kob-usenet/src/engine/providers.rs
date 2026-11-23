use std::sync::{Arc, Mutex, PoisonError, RwLock};

use tokio::runtime::Handle;

use crate::config::ServerConfig;
use crate::providers::{Accounts, Shape};
use crate::store::Store;
use crate::vendor::nntp::Client;
use crate::{Error, day};

pub struct Providers {
    pub client: Arc<Client>,
    pub store: Arc<Store>,
    servers: RwLock<Vec<ServerConfig>>,
    accounts: Accounts,
    shape: Shape,
    built: Mutex<Vec<String>>,
    applying: Mutex<()>,
    runtime: Handle,
}

impl Providers {
    pub fn new(store: Arc<Store>, servers: Vec<ServerConfig>, shape: Shape) -> Result<Self, Error> {
        let accounts = Accounts::new(store.usage()?);
        let providers = accounts.build(&servers, &shape)?;
        let built = providers.iter().map(|p| p.key.clone()).collect();
        Ok(Self {
            client: Arc::new(Client::new(providers)),
            store,
            servers: RwLock::new(servers),
            accounts,
            shape,
            built: Mutex::new(built),
            applying: Mutex::new(()),
            runtime: Handle::current(),
        })
    }

    pub fn reconfigure(&self, servers: Vec<ServerConfig>) -> Result<(), Error> {
        let _applying = self.applying.lock().unwrap_or_else(PoisonError::into_inner);
        self.apply(servers)
    }

    fn apply(&self, servers: Vec<ServerConfig>) -> Result<(), Error> {
        let _runtime = self.runtime.enter();
        let providers = self.accounts.build(&servers, &self.shape)?;
        let built = providers.iter().map(|p| p.key.clone()).collect();
        self.client.replace(providers);
        *self.built.lock().unwrap_or_else(PoisonError::into_inner) = built;
        *self.servers.write().unwrap_or_else(PoisonError::into_inner) = servers;
        Ok(())
    }

    pub fn refresh(&self) -> Result<(), Error> {
        let _applying = self.applying.lock().unwrap_or_else(PoisonError::into_inner);
        let servers = self
            .servers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let usable: Vec<String> = self
            .accounts
            .usable(&servers)
            .iter()
            .map(|s| s.id.clone())
            .collect();
        if *self.built.lock().unwrap_or_else(PoisonError::into_inner) == usable {
            return Ok(());
        }
        tracing::info!(
            ?usable,
            "providers expired or used up their quota; rebuilding"
        );
        self.apply(servers)
    }

    pub fn retains(&self, posted: Option<i64>) -> bool {
        let Some(posted) = posted else {
            return true;
        };
        let now = day::now();
        let servers = self.servers.read().unwrap_or_else(PoisonError::into_inner);
        let usable = self.accounts.usable(&servers);
        usable.is_empty() || usable.iter().any(|s| s.retains(posted, now))
    }

    pub fn save_usage(&self) {
        if let Err(error) = self.store.put_usage(&self.accounts.used()) {
            tracing::warn!(%error, "could not save provider usage");
        }
    }
}
