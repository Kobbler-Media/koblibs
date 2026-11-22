pub(crate) mod fetch;
mod plan;
mod probe;
mod run;
mod store;

use std::collections::HashMap;
use std::path::PathBuf;
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
    gate: SpaceGate,
    slots: Arc<Semaphore>,
    status: Mutex<HashMap<String, (u64, JobStatus)>>,
    tasks: Mutex<HashMap<String, JoinHandle<()>>>,
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
        let gate = SpaceGate::new(Arc::clone(&config.free_space), config.free_space_floor);
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
        let seq = {
            let status = self
                .shared
                .status
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if status.contains_key(&spec.id) {
                return Err(Error::DuplicateJob(spec.id));
            }
            status.values().map(|(seq, _)| seq + 1).max().unwrap_or(0)
        };
        let stored = StoredSpec::new(seq, &spec);
        let status = JobStatus {
            id: spec.id.clone(),
            name: spec.name.clone(),
            category: spec.category.clone(),
            state: JobState::Queued,
            bytes_total: 0,
            bytes_done: 0,
            segments_total: 0,
            segments_done: 0,
            error: None,
            path: stored.staging(),
        };
        self.shared.store.create(&stored, &spec.nzb, &status)?;
        self.shared
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(spec.id, (seq, status.clone()));
        self.spawn(stored);
        Ok(status)
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
        if delete_files {
            remove_dir(&spec.staging())?;
            if status.state == JobState::Completed {
                remove_dir(&status.path)?;
            }
        }
        Ok(())
    }

    pub fn paused_for_space(&self) -> bool {
        self.shared.gate.paused()
    }

    fn spawn(&self, spec: StoredSpec) {
        let shared = Arc::clone(&self.shared);
        let id = spec.id.clone();
        let task = tokio::spawn(async move {
            let Ok(_slot) = Arc::clone(&shared.slots).acquire_owned().await else {
                return;
            };
            run::run(&shared, &spec).await;
        });
        self.shared
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, task);
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
