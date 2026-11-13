use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use tokio::task::Id;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningJob {
    pub job_type: String,
    pub job_id: Uuid,
}

static RUNNING: LazyLock<Mutex<HashMap<Id, RunningJob>>> = LazyLock::new(Default::default);

/// Which job the task `task` is executing, so a watchdog that sees a task stuck in one poll
/// can name the job instead of the line that spawned every job.
pub fn running_job(task: Id) -> Option<RunningJob> {
    RUNNING.lock().ok()?.get(&task).cloned()
}

#[must_use = "the job is only named while the guard lives"]
pub struct RunningGuard(Option<Id>);

pub fn track_running(job_type: &str, job_id: Uuid) -> RunningGuard {
    let Some(task) = tokio::task::try_id() else {
        return RunningGuard(None);
    };
    if let Ok(mut running) = RUNNING.lock() {
        running.insert(
            task,
            RunningJob {
                job_type: job_type.to_string(),
                job_id,
            },
        );
    }
    RunningGuard(Some(task))
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        if let Some(task) = self.0
            && let Ok(mut running) = RUNNING.lock()
        {
            running.remove(&task);
        }
    }
}
