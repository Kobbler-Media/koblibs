use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::config::{EngineConfig, ServerConfig};
use crate::flight::Flights;
use crate::providers::{Accounts, Shape};
use crate::reader::OutputReader;
use crate::release::{OutputInfo, Release, ReleaseKey};
use crate::store::{Layout, Store};
use crate::vendor::nntp::{Client, ProviderStats};
use crate::vendor::queue::import::{Imported, Progress, Rules, import};
use crate::vendor::queue::nzb;
use crate::{Error, ImportMode, Purpose, day};

const WARM_EVERY: Duration = Duration::from_secs(30);

pub struct Engine {
    client: Arc<Client>,
    servers: RwLock<Vec<ServerConfig>>,
    accounts: Accounts,
    shape: Shape,
    store: Store,
    flights: Flights,
    layouts: Mutex<HashMap<ReleaseKey, Arc<Layout>>>,
    warm: JoinHandle<()>,
}

impl Engine {
    pub async fn new(config: EngineConfig) -> Result<Self, Error> {
        let store = Store::open(&config.data_dir, config.nzb_cache_bytes)?;
        let accounts = Accounts::new(store.usage());
        let shape = Shape {
            pipeline_depth: config.pipeline_depth,
            min_connections: config.min_connections,
        };
        let client = Arc::new(Client::new(accounts.build(&config.servers, &shape)?));
        let warm = tokio::spawn(keep_warm(Arc::clone(&client)));
        Ok(Self {
            client,
            servers: RwLock::new(config.servers),
            accounts,
            shape,
            store,
            flights: Flights::new(config.read_ahead_bytes, config.read_budget_bytes),
            layouts: Mutex::default(),
            warm,
        })
    }

    pub fn reconfigure(&self, servers: Vec<ServerConfig>) -> Result<(), Error> {
        self.client
            .replace(self.accounts.build(&servers, &self.shape)?);
        *self.servers.write().unwrap_or_else(PoisonError::into_inner) = servers;
        Ok(())
    }

    pub async fn prewarm(&self) {
        self.client.warm().await;
    }

    pub async fn import(
        &self,
        nzb: Option<&[u8]>,
        cache_key: Option<&str>,
        name: &str,
        password: Option<&str>,
        mode: ImportMode,
    ) -> Result<Release, Error> {
        let aliased = cache_key.and_then(|k| self.store.alias(k));
        if let Some(release) = aliased
            .as_ref()
            .map(|key| self.known(key))
            .transpose()?
            .flatten()
        {
            return Ok(release);
        }
        let xml = match nzb {
            Some(raw) => nzb::decompress(raw)?.into_owned(),
            None => aliased
                .and_then(|key| self.store.nzb(&key))
                .ok_or(Error::NotCached)?,
        };
        let parsed = nzb::parse(&xml)?;
        let key = ReleaseKey::of(&parsed);
        if let Some(cache_key) = cache_key {
            self.store.set_alias(cache_key, &key)?;
        }
        if let Some(release) = self.known(&key)? {
            return Ok(release);
        }
        self.store.put_nzb(&key, &xml)?;
        let posted = parsed.files.iter().filter_map(|f| f.date).max();
        self.check_retention(posted)?;
        let rules = Rules {
            check_percent: mode.check_percent(),
            concurrency: mode.concurrency(),
            ..Rules::default()
        };
        let password = password.or_else(|| parsed.meta("password").next());
        let imported = import(
            &self.client,
            &parsed,
            name,
            "",
            &rules,
            password,
            Arc::new(Progress::default()),
            None,
        )
        .await;
        self.save_usage();
        let Imported::Files { outputs, .. } = imported? else {
            return Err(Error::NestedNzb);
        };
        let mut outputs: Vec<_> = outputs
            .into_iter()
            .map(|o| (o.path.join("/"), o.meta))
            .collect();
        outputs.sort_by(|a, b| a.0.cmp(&b.0));
        let layout = Layout { posted, outputs };
        self.store.put_layout(&key, &layout)?;
        let release = describe(&key, &layout);
        self.layouts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, Arc::new(layout));
        Ok(release)
    }

    pub fn release(&self, key: &ReleaseKey) -> Result<Release, Error> {
        self.known(key)?
            .ok_or_else(|| Error::UnknownRelease(key.clone()))
    }

    pub fn open(
        &self,
        key: &ReleaseKey,
        output: usize,
        purpose: Purpose,
    ) -> Result<OutputReader, Error> {
        let layout = self
            .layout(key)?
            .ok_or_else(|| Error::UnknownRelease(key.clone()))?;
        let (_, meta) = layout
            .outputs
            .get(output)
            .ok_or_else(|| Error::UnknownOutput {
                key: key.clone(),
                index: output,
            })?;
        let reader = self.flights.reader(&self.client, meta, purpose)?;
        let reader = match layout.posted {
            Some(posted) => reader.with_posted(posted),
            None => reader,
        };
        Ok(OutputReader::new(reader, meta.size()))
    }

    #[must_use]
    pub fn stats(&self) -> Vec<ProviderStats> {
        self.client.stats()
    }

    fn known(&self, key: &ReleaseKey) -> Result<Option<Release>, Error> {
        Ok(self.layout(key)?.map(|layout| describe(key, &layout)))
    }

    fn layout(&self, key: &ReleaseKey) -> Result<Option<Arc<Layout>>, Error> {
        if let Some(layout) = self
            .layouts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
        {
            return Ok(Some(Arc::clone(layout)));
        }
        let Some(layout) = self.store.layout(key)?.map(Arc::new) else {
            return Ok(None);
        };
        self.layouts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone(), Arc::clone(&layout));
        Ok(Some(layout))
    }

    fn check_retention(&self, posted: Option<i64>) -> Result<(), Error> {
        let Some(posted) = posted else {
            return Ok(());
        };
        let (posted, today) = (day::of_unix(posted), day::today());
        let servers = self.servers.read().unwrap_or_else(PoisonError::into_inner);
        let usable = self.accounts.usable(&servers);
        if !usable.is_empty() && usable.iter().all(|s| !s.retains(posted, today)) {
            return Err(Error::Retention);
        }
        Ok(())
    }

    fn save_usage(&self) {
        if let Err(error) = self.store.put_usage(&self.accounts.used()) {
            tracing::warn!(%error, "could not save provider usage");
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.warm.abort();
        self.save_usage();
    }
}

fn describe(key: &ReleaseKey, layout: &Layout) -> Release {
    Release {
        key: key.clone(),
        outputs: layout
            .outputs
            .iter()
            .enumerate()
            .map(|(index, (path, meta))| OutputInfo {
                index,
                path: path.clone(),
                size: meta.size(),
            })
            .collect(),
    }
}

async fn keep_warm(client: Arc<Client>) {
    loop {
        client.warm().await;
        tokio::time::sleep(WARM_EVERY).await;
    }
}
