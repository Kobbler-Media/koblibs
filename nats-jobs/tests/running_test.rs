use nats_jobs::running::{RunningJob, running_job, track_running};
use uuid::Uuid;

#[tokio::test]
async fn a_task_running_a_job_can_be_named_until_it_finishes() {
    let job_id = Uuid::new_v4();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _running = track_running("kobload_ingest", job_id);
        let _ = rx.await;
    });
    tokio::task::yield_now().await;
    while running_job(task.id()).is_none() {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        running_job(task.id()),
        Some(RunningJob {
            job_type: "kobload_ingest".into(),
            job_id
        })
    );

    let id = task.id();
    tx.send(()).unwrap();
    task.await.unwrap();
    assert_eq!(running_job(id), None);
}
