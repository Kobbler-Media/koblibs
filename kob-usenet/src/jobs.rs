pub(crate) mod failure;
pub(crate) mod fetch;
mod finish;
mod headless;
mod payload;
mod plan;
mod probe;
mod raw;
mod run;
mod settle;
mod store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
#[cfg(feature = "testkit")]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::Error;
use crate::config::DownloadConfig;
use crate::space::SpaceGate;
use crate::vendor::nntp::Client;
pub(crate) use plan::password_failure;
use store::{JobStore, StoredSpec};

#[derive(Clone)]
pub struct JobSpec {
    pub id: String,
    pub name: String,
    pub category: String,
    pub nzb: Vec<u8>,
    pub password: Option<String>,
    pub dest_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobState {
    Queued,
    Downloading,
    Verifying,
    Repairing,
    Unpacking,
    Completed,
    Failed,
}

impl JobState {
    #[must_use]
    pub const fn is_finished(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobFailure {
    Content,
    Infrastructure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStatus {
    pub id: String,
    pub name: String,
    pub category: String,
    pub state: JobState,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub segments_total: u64,
    pub segments_done: u64,
    pub error: Option<String>,
    pub failure: Option<JobFailure>,
    pub path: PathBuf,
}

impl JobStatus {
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "a progress fraction needs no more than f64 precision"
    )]
    pub fn progress(&self) -> f64 {
        if self.state == JobState::Completed {
            return 1.0;
        }
        if self.bytes_total == 0 {
            return 0.0;
        }
        self.bytes_done as f64 / self.bytes_total as f64
    }
}

pub(crate) struct Jobs {
    shared: Arc<Shared>,
}

struct Shared {
    client: Arc<Client>,
    config: DownloadConfig,
    pipeline_depth: usize,
    store: JobStore,
    gate: Arc<SpaceGate>,
    slots: Arc<Semaphore>,
    status: Mutex<HashMap<String, (u64, JobStatus)>>,
    tasks: Mutex<HashMap<String, JoinHandle<()>>>,
    solving: Arc<AtomicUsize>,
}

impl Shared {
    fn status(&self, id: &str) -> Option<JobStatus> {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .map(|(_, s)| s.clone())
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut JobStatus)) -> Option<JobStatus> {
        let mut status = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        let (_, entry) = status.get_mut(id)?;
        change(entry);
        Some(entry.clone())
    }

    fn set_state(&self, id: &str, change: impl FnOnce(&mut JobStatus)) {
        if let Some(status) = self.update(id, change)
            && let Err(error) = self.store.put_status(&status)
        {
            tracing::warn!(%error, job = id, "could not save job status");
        }
    }
}

impl Jobs {
    pub fn open(
        root: &std::path::Path,
        client: Arc<Client>,
        config: DownloadConfig,
        pipeline_depth: usize,
    ) -> Result<Self, Error> {
        let store = JobStore::open(root)?;
        let gate = Arc::new(SpaceGate::new(
            Arc::clone(&config.free_space),
            config.free_space_floor,
        ));
        let slots = Arc::new(Semaphore::new(config.active_jobs.max(1)));
        let shared = Arc::new(Shared {
            client,
            config,
            pipeline_depth,
            store,
            gate,
            slots,
            status: Mutex::default(),
            tasks: Mutex::default(),
            solving: Arc::default(),
        });
        let mut loaded = shared.store.load()?;
        loaded.sort_by_key(|(spec, _)| spec.seq);
        let jobs = Self { shared };
        for (spec, mut status) in loaded {
            let resume = !status.state.is_finished();
            if resume {
                status.state = JobState::Queued;
            }
            jobs.shared
                .status
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(spec.id.clone(), (spec.seq, status));
            if resume {
                jobs.spawn(spec);
            }
        }
        Ok(jobs)
    }

    pub fn add(&self, spec: JobSpec) -> Result<JobStatus, Error> {
        let mut status = self
            .shared
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if status.contains_key(&spec.id) {
            return Err(Error::DuplicateJob(spec.id));
        }
        let seq = status.values().map(|(seq, _)| seq + 1).max().unwrap_or(0);
        let stored = StoredSpec::new(seq, &spec);
        let created = JobStatus {
            id: spec.id.clone(),
            name: spec.name.clone(),
            category: spec.category.clone(),
            state: JobState::Queued,
            bytes_total: 0,
            bytes_done: 0,
            segments_total: 0,
            segments_done: 0,
            error: None,
            failure: None,
            path: stored.staging(),
        };
        self.shared.store.create(&stored, &spec.nzb, &created)?;
        status.insert(spec.id, (seq, created.clone()));
        drop(status);
        self.spawn(stored);
        Ok(created)
    }

    pub fn list(&self) -> Vec<JobStatus> {
        let status = self
            .shared
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut all: Vec<&(u64, JobStatus)> = status.values().collect();
        all.sort_by_key(|(seq, _)| *seq);
        all.into_iter().map(|(_, s)| s.clone()).collect()
    }

    pub fn get(&self, id: &str) -> Option<JobStatus> {
        self.shared.status(id)
    }

    pub async fn remove(&self, id: &str, delete_files: bool) -> Result<(), Error> {
        let Some(status) = self.shared.status(id) else {
            return Err(Error::UnknownJob(id.to_owned()));
        };
        let task = self
            .shared
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        let spec = self.shared.store.spec(id)?;
        self.shared.store.remove(id)?;
        self.shared
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        remove_dir(&spec.staging())?;
        if delete_files && status.state == JobState::Completed {
            remove_dir(&status.path)?;
        }
        Ok(())
    }

    pub fn paused_for_space(&self) -> bool {
        self.shared.gate.paused()
    }

    #[cfg(feature = "testkit")]
    pub fn repairs_running(&self) -> usize {
        self.shared.solving.load(Ordering::Acquire)
    }

    fn spawn(&self, spec: StoredSpec) {
        let shared = Arc::clone(&self.shared);
        let id = spec.id.clone();
        let mut tasks = self
            .shared
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let task = tokio::spawn(async move {
            work(&shared, &spec).await;
            let mut tasks = shared.tasks.lock().unwrap_or_else(PoisonError::into_inner);
            if tasks
                .get(&spec.id)
                .is_some_and(|t| t.id() == tokio::task::id())
            {
                tasks.remove(&spec.id);
            }
        });
        tasks.insert(id, task);
    }
}

async fn work(shared: &Shared, spec: &StoredSpec) {
    {
        let mut wait = shared.config.retry_after;
        for attempt in 1.. {
            let Ok(slot) = Arc::clone(&shared.slots).acquire_owned().await else {
                return;
            };
            let slot = Arc::new(slot);
            let last_try = attempt >= shared.config.retry_attempts.max(1);
            if matches!(
                run::run(shared, spec, last_try, &slot).await,
                run::Ended::Finished
            ) {
                return;
            }
            drop(slot);
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(shared.config.retry_max);
        }
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        for (_, task) in self
            .shared
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
        {
            task.abort();
        }
    }
}

fn remove_dir(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}
