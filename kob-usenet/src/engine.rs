mod admission;
mod locks;
mod maintenance;
mod providers;

use std::sync::Arc;

use quick_cache::sync::Cache;
use tokio::task::JoinHandle;

use crate::config::{EngineConfig, ServerConfig};
use crate::flight::Flights;
use crate::health::{self, HealthSample};
use crate::jobs::{JobSpec, JobStatus, Jobs};
use crate::providers::Shape;
use crate::reader::OutputReader;
use crate::release::{OutputInfo, Release, ReleaseKey};
use crate::store::{Layout, Store};
use crate::vendor::nntp::{ProviderStats, StageMetrics};
use crate::vendor::queue::import::{Error as ImportError, Imported, Progress, Rules, import};
use crate::vendor::queue::nzb;
use crate::{Error, ImportMode, Purpose, verify};
use admission::Admission;
use locks::KeyedLocks;
use providers::Providers;

const LAYOUTS: usize = 512;

pub struct Engine {
    providers: Arc<Providers>,
    flights: Flights,
    layouts: Cache<ReleaseKey, Arc<Layout>>,
    imports: Admission,
    importing: KeyedLocks,
    maintenance: JoinHandle<()>,
    jobs: Jobs,
}

impl Engine {
    pub async fn new(config: EngineConfig) -> Result<Self, Error> {
        tracing::info!(
            memory_budget_bytes = config.memory_budget_bytes(),
            read_budget_bytes = config.read_budget_bytes,
            cache_read_ahead_bytes = config.cache_read_ahead_bytes,
            flight_cache_bytes = config.flight_cache_bytes,
            repair_memory_bytes = config.download.repair_memory_bytes,
            "engine memory budget"
        );
        let store = Arc::new(Store::open(
            &config.data_dir,
            config.nzb_cache_bytes,
            config.release_cache_bytes,
        )?);
        let shape = Shape {
            pipeline_depth: config.pipeline_depth,
            min_connections: config.min_connections,
            timeout: config.provider_timeout,
        };
        let providers = Arc::new(Providers::new(store, config.servers, shape)?);
        let maintenance = tokio::spawn(maintenance::run(
            Arc::clone(&providers),
            config.maintenance_interval,
        ));
        let jobs = Jobs::open(
            &config.data_dir,
            Arc::clone(&providers.client),
            config.download,
            config.pipeline_depth,
        )?;
        Ok(Self {
            providers,
            flights: Flights::new(
                config.read_ahead_bytes,
                config.cache_read_ahead_bytes,
                config.flight_cache_bytes,
                config.read_budget_bytes,
            ),
            layouts: Cache::new(LAYOUTS),
            imports: Admission::default(),
            importing: KeyedLocks::default(),
            maintenance,
            jobs,
        })
    }

    pub fn reconfigure(&self, servers: Vec<ServerConfig>) -> Result<(), Error> {
        self.providers.reconfigure(servers)
    }

    pub async fn prewarm(&self) {
        self.providers.client.warm().await;
    }

    pub async fn import(
        &self,
        nzb: Option<&[u8]>,
        cache_key: Option<&str>,
        name: &str,
        password: Option<&str>,
        mode: ImportMode,
    ) -> Result<Release, Error> {
        let store = &self.providers.store;
        let aliased = cache_key.and_then(|k| store.alias(k));
        if let Some(release) = aliased
            .as_ref()
            .map(|k| self.known(k))
            .transpose()?
            .flatten()
        {
            return Ok(release);
        }
        let xml = match nzb {
            Some(raw) => nzb::decompress(raw)?.into_owned(),
            None => aliased
                .and_then(|key| store.nzb(&key))
                .ok_or(Error::NotCached)?,
        };
        let parsed = nzb::parse(&xml)?;
        let key = ReleaseKey::of(&parsed);
        if let Some(cache_key) = cache_key {
            store.set_alias(cache_key, &key)?;
        }
        let _importing = self.importing.lock(key.as_str()).await;
        if let Some(release) = self.known(&key)? {
            return Ok(release);
        }
        store.put_nzb(&key, &xml)?;
        let posted = parsed.files.iter().filter_map(|f| f.date).max();
        if !self.providers.retains(posted) {
            return Err(Error::Retention);
        }
        let rules = Rules {
            check_percent: mode.check_percent(),
            concurrency: mode.concurrency(),
            workload: mode.workload(),
            ..Rules::default()
        };
        let password = password.or_else(|| parsed.meta("password").next());
        let imported = {
            let _slot = self.imports.enter(mode).await;
            import(
                &self.providers.client,
                &parsed,
                name,
                "",
                &rules,
                password,
                Arc::new(Progress::default()),
                None,
            )
            .await
        };
        self.providers.save_usage();
        let Imported::Files { outputs, .. } = imported? else {
            return Err(Error::NestedNzb);
        };
        let mut outputs: Vec<_> = outputs
            .into_iter()
            .map(|o| (o.path.join("/"), o.meta))
            .collect();
        outputs.sort_by(|a, b| a.0.cmp(&b.0));
        let mut kept = Vec::with_capacity(outputs.len());
        let mut dropped = None;
        for (path, meta) in outputs {
            if !meta.encrypted() {
                kept.push((path, meta));
                continue;
            }
            let reader = self.flights.import_reader(&self.providers.client, &meta)?;
            match verify::decrypted(&path, reader, meta.size()).await {
                Ok(()) => kept.push((path, meta)),
                Err(Error::Import(ImportError::Unsupported(reason))) => {
                    tracing::info!(%path, "dropping an encrypted non-media output from a stream release");
                    dropped = Some(reason);
                }
                Err(error) => return Err(error),
            }
        }
        if let (true, Some(reason)) = (kept.is_empty(), dropped) {
            return Err(Error::Import(ImportError::Unsupported(reason)));
        }
        let layout = Layout {
            posted,
            outputs: kept,
        };
        store.put_layout(&key, &layout)?;
        let release = describe(&key, &layout);
        self.layouts.insert(key, Arc::new(layout));
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
        let reader = self.flights.reader(&self.providers.client, meta, purpose)?;
        let reader = match layout.posted {
            Some(posted) => reader.with_posted(posted),
            None => reader,
        };
        Ok(OutputReader::new(reader, meta.size()))
    }

    pub async fn sample_health(
        &self,
        key: &ReleaseKey,
        percent: u8,
        output: Option<usize>,
    ) -> Result<HealthSample, Error> {
        let layout = self.checkable(key, output)?;
        health::sample(&self.providers.client, &layout, percent, output).await
    }

    pub async fn quick_check(
        &self,
        key: &ReleaseKey,
        articles: usize,
        output: Option<usize>,
    ) -> Result<HealthSample, Error> {
        let layout = self.checkable(key, output)?;
        health::quick(&self.providers.client, &layout, articles, output).await
    }

    fn checkable(&self, key: &ReleaseKey, output: Option<usize>) -> Result<Arc<Layout>, Error> {
        let layout = self
            .layout(key)?
            .ok_or_else(|| Error::UnknownRelease(key.clone()))?;
        if let Some(index) = output
            && index >= layout.outputs.len()
        {
            return Err(Error::UnknownOutput {
                key: key.clone(),
                index,
            });
        }
        Ok(layout)
    }

    pub async fn add_job(&self, spec: JobSpec) -> Result<JobStatus, Error> {
        self.jobs.add(spec)
    }

    #[must_use]
    pub fn jobs(&self) -> Vec<JobStatus> {
        self.jobs.list()
    }

    #[must_use]
    pub fn job(&self, id: &str) -> Option<JobStatus> {
        self.jobs.get(id)
    }

    pub async fn remove_job(&self, id: &str, delete_files: bool) -> Result<(), Error> {
        self.jobs.remove(id, delete_files).await
    }

    #[must_use]
    #[cfg(feature = "testkit")]
    pub fn repairs_running(&self) -> usize {
        self.jobs.repairs_running()
    }

    pub fn paused_for_space(&self) -> bool {
        self.jobs.paused_for_space()
    }

    #[must_use]
    pub fn stats(&self) -> Vec<ProviderStats> {
        self.providers.client.stats()
    }

    #[must_use]
    pub fn stream_stages(&self) -> Arc<StageMetrics> {
        self.providers.client.stream_stages()
    }

    fn known(&self, key: &ReleaseKey) -> Result<Option<Release>, Error> {
        Ok(self.layout(key)?.map(|layout| describe(key, &layout)))
    }

    fn layout(&self, key: &ReleaseKey) -> Result<Option<Arc<Layout>>, Error> {
        if let Some(layout) = self.layouts.get(key) {
            self.providers.store.used_layout(key);
            return Ok(Some(layout));
        }
        let Some(layout) = self.providers.store.layout(key)?.map(Arc::new) else {
            return Ok(None);
        };
        self.layouts.insert(key.clone(), Arc::clone(&layout));
        Ok(Some(layout))
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.maintenance.abort();
        self.providers.save_usage();
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
