use crate::client::NatsError;
use async_nats::jetstream::consumer::Consumer;
use async_nats::jetstream::consumer::pull::Config as ConsumerConfig;
use async_nats::jetstream::stream::Stream;
use tracing::warn;

pub(crate) async fn ensure_consumer(
    stream: &mut Stream,
    consumer_name: &str,
    config: &ConsumerConfig,
) -> Result<Consumer<ConsumerConfig>, NatsError> {
    if let Err(e) = delete_if_ahead_of_stream(stream, consumer_name).await {
        warn!(
            "Error checking stale consumer '{}': {}, continuing anyway",
            consumer_name, e
        );
    }
    // Create-or-update: a changed ack_wait/max_deliver/max_ack_pending/backoff is
    // edited in place, so in-flight messages keep their ack state rather than
    // being redelivered by a recreated consumer.
    stream
        .create_consumer(config.clone())
        .await
        .map_err(|e| NatsError::Consumer(e.to_string()))
}

/// A consumer whose delivered sequence is past the end of the stream will never
/// see another message, so it has to be recreated from scratch.
async fn delete_if_ahead_of_stream(
    stream: &mut Stream,
    consumer_name: &str,
) -> Result<(), NatsError> {
    let stream_last_seq = stream
        .info()
        .await
        .map_err(|e| NatsError::JetStream(e.to_string()))?
        .state
        .last_sequence;

    if let Ok(mut existing) = stream.get_consumer::<ConsumerConfig>(consumer_name).await
        && let Ok(info) = existing.info().await
        && info.delivered.stream_sequence > stream_last_seq
        && stream_last_seq > 0
    {
        warn!(
            "Consumer '{}' is stale (at seq {} but stream ends at {}), recreating",
            consumer_name, info.delivered.stream_sequence, stream_last_seq
        );
        if let Err(e) = stream.delete_consumer(consumer_name).await {
            warn!("Failed to delete stale consumer '{}': {}", consumer_name, e);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{NatsClient, NatsConfig};
    use async_nats::jetstream::consumer::AckPolicy;
    use std::time::Duration;

    async fn test_nats() -> Option<NatsClient> {
        let url = std::env::var("NATS_URL_TEST").ok()?;
        Some(
            NatsClient::connect(&NatsConfig::new(url, "nats-jobs-consumer-test"))
                .await
                .expect("connect to NATS_URL_TEST"),
        )
    }

    fn config(name: &str, max_deliver: i64, ack_wait: Duration) -> ConsumerConfig {
        ConsumerConfig {
            durable_name: Some(name.to_string()),
            filter_subject: format!("jobs.{name}.probe"),
            ack_policy: AckPolicy::Explicit,
            ack_wait,
            max_deliver,
            max_ack_pending: 2,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn config_drift_updates_the_consumer_in_place() {
        let Some(nats) = test_nats().await else {
            eprintln!("NATS_URL_TEST unset, skipping");
            return;
        };
        let name = format!("consumer-test-{}", uuid::Uuid::new_v4().simple());
        let mut stream = nats.jetstream().get_stream("JOBS").await.unwrap();

        let mut first = ensure_consumer(
            &mut stream,
            &name,
            &config(&name, 3, Duration::from_secs(30)),
        )
        .await
        .unwrap();
        let created = first.info().await.unwrap().created;

        let mut second = ensure_consumer(
            &mut stream,
            &name,
            &config(&name, 5, Duration::from_secs(60)),
        )
        .await
        .unwrap();
        let info = second.info().await.unwrap().clone();
        stream.delete_consumer(&name).await.unwrap();

        assert_eq!(info.config.max_deliver, 5);
        assert_eq!(info.config.ack_wait, Duration::from_secs(60));
        assert_eq!(info.created, created, "consumer was recreated, not updated");
    }
}
