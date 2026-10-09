# nats-jobs

Shared NATS JetStream job system for Kobbler and Kobload.

## Overview

This crate provides a unified job queue and event system using NATS JetStream:

- **JOBS stream** (work queue): Each job is processed by exactly one worker
- **EVENTS stream** (pub/sub): Events can be consumed by multiple subscribers
- **Request/Reply**: For synchronous operations like search

## Usage

```rust
use nats_jobs::{NatsClient, NatsConfig, Publisher, JobWorker, WorkerConfig, JobHandler};

// Connect to NATS
let config = NatsConfig::from_env("my-service");
let nats = NatsClient::connect(&config).await?;

// Enqueue a job
let publisher = nats.publisher();
publisher.enqueue_job_with_context(
    "kobbler",
    "directory_scan",
    serde_json::json!({ "watch_directory_id": "..." }),
    JobContext::new("Scanning /books directory"),
).await?;

// Publish an event
publisher.publish_event(
    "download.completed",
    serde_json::json!({ "download_id": "..." }),
).await?;
```

## Components

### NatsClient

Connection management with automatic stream setup:

```rust
let config = NatsConfig::from_env("kobbler");
let nats = NatsClient::connect(&config).await?;
```

### Publisher

Enqueue jobs and publish events:

```rust
let publisher = Publisher::new(nats.clone(), "kobbler");

// Job with context (for UI visibility)
publisher.enqueue_job_with_context(
    "kobbler",           // target service
    "metadata_write",    // job type
    payload,             // JSON payload
    JobContext::for_entity("book", book_id, "Writing metadata"),
).await?;

// Event
publisher.publish_event("library.book_added", payload).await?;
```

### JobWorker

Process jobs from the queue:

```rust
let mut worker = JobWorker::new(nats, WorkerConfig::for_service("kobbler"))
    .with_job_db(JobStatusDb::new(pool));

worker.register_handler(MyHandler::new());

worker.run(shutdown_rx).await?;
```

### JobHandler Trait

Implement for each job type:

```rust
#[async_trait]
impl JobHandler for MyHandler {
    fn job_type(&self) -> &'static str {
        "my_job_type"
    }

    async fn execute(&self, job_id: Uuid, payload: Value) -> Result<Value, JobError> {
        let payload: MyPayload = serde_json::from_value(payload)?;
        // Process job...
        Ok(serde_json::json!({ "processed": true }))
    }
}
```

### JobContext

Provides UI visibility for running jobs:

```rust
// Simple description
JobContext::new("Processing batch import")

// Linked to an entity (shows on entity's page in UI)
JobContext::for_entity("book", book_id, "Writing KEPUB metadata")
```

### JobError

Distinguishes retryable vs permanent errors:

```rust
// Permanent failure - won't retry
JobError::permanent("Invalid payload format")

// Retryable - will retry with backoff
JobError::retryable("Network timeout")
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `NATS_URL` | `nats://localhost:4222` | NATS server URL |

## Database

Jobs are tracked in a local `job_status` table for UI queries:

```sql
CREATE TABLE job_status (
    id UUID PRIMARY KEY,
    job_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    payload JSONB,
    result JSONB,
    error TEXT,
    progress SMALLINT,
    entity_type TEXT,
    entity_id UUID,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);
```

## Cross-Service Communication

### Kobbler → Kobload

Watch pattern sync via jobs:
- `jobs.kobload.watch_pattern_upsert`
- `jobs.kobload.watch_pattern_delete`

### Kobload → Kobbler

Download events:
- `events.download.completed`
- `events.download.failed`
