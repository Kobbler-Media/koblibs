//! NATS client wrapper with connection management and stream setup.

use async_nats::jetstream::{self, stream::Config as StreamConfig, stream::RetentionPolicy};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

/// Configuration for NATS connection.
#[derive(Debug, Clone)]
pub struct NatsConfig {
    /// NATS server URL (e.g., "nats://localhost:4222")
    pub url: String,
    /// Optional token for authentication (maps to NATS --auth)
    pub token: Option<String>,
    /// Optional username for authentication
    pub username: Option<String>,
    /// Optional password for authentication
    pub password: Option<String>,
    /// Optional NKey seed for authentication
    pub nkey_seed: Option<String>,
    /// Client name for debugging
    pub client_name: String,
}

impl NatsConfig {
    /// Create a new config with just the URL.
    pub fn new(url: impl Into<String>, client_name: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            token: None,
            username: None,
            password: None,
            nkey_seed: None,
            client_name: client_name.into(),
        }
    }

    /// Build config from environment variables.
    /// If NATS_URL contains a userinfo component (e.g. nats://token@host:4222),
    /// it is extracted as the auth token automatically.
    pub fn from_env(client_name: impl Into<String>) -> Self {
        let raw_url =
            std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".to_string());
        let (url, url_token) = extract_userinfo(&raw_url);
        let token = std::env::var("NATS_TOKEN").ok().or(url_token);

        Self {
            url,
            token,
            username: std::env::var("NATS_USER").ok(),
            password: std::env::var("NATS_PASSWORD").ok(),
            nkey_seed: std::env::var("NATS_NKEY_SEED").ok(),
            client_name: client_name.into(),
        }
    }
}

/// Extract userinfo (token) from a NATS URL, returning the cleaned URL and the token.
/// e.g. "nats://mytoken@host:4222" -> ("nats://host:4222", Some("mytoken"))
fn extract_userinfo(url: &str) -> (String, Option<String>) {
    // Find the :// prefix
    let Some(scheme_end) = url.find("://") else {
        return (url.to_string(), None);
    };
    let after_scheme = &url[scheme_end + 3..];

    // Check for @ in the authority (before any /)
    let authority_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];

    let Some(at_pos) = authority.find('@') else {
        return (url.to_string(), None);
    };

    let userinfo = &authority[..at_pos];
    let host_and_rest = &after_scheme[at_pos + 1..];
    let clean_url = format!("{}{}", &url[..scheme_end + 3], host_and_rest);

    if userinfo.is_empty() {
        (clean_url, None)
    } else {
        (clean_url, Some(userinfo.to_string()))
    }
}

#[derive(Error, Debug)]
pub enum NatsError {
    #[error("NATS connection error: {0}")]
    Connection(#[from] async_nats::ConnectError),
    #[error("NATS subscribe error: {0}")]
    Subscribe(#[from] async_nats::SubscribeError),
    #[error("NATS publish error: {0}")]
    Publish(#[from] async_nats::PublishError),
    #[error("NATS request error: {0}")]
    Request(#[from] async_nats::RequestError),
    #[error("JetStream error: {0}")]
    JetStream(String),
    #[error("JetStream context error: {0}")]
    JetStreamContext(#[from] async_nats::jetstream::context::CreateStreamError),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Consumer error: {0}")]
    Consumer(String),
}

/// NATS client wrapper providing connection management and JetStream access.
#[derive(Clone)]
pub struct NatsClient {
    client: async_nats::Client,
    jetstream: jetstream::Context,
    /// Broadcast channel for reconnection events.
    reconnect_tx: Arc<broadcast::Sender<()>>,
}

impl NatsClient {
    /// Connect to NATS and set up JetStream.
    pub async fn connect(config: &NatsConfig) -> Result<Self, NatsError> {
        // Create broadcast channel for reconnection events
        let (reconnect_tx, _) = broadcast::channel(16);
        let tx_clone = reconnect_tx.clone();

        let mut connect_options = async_nats::ConnectOptions::new()
            .name(&config.client_name)
            .retry_on_initial_connect()
            .event_callback(move |event| {
                let tx = tx_clone.clone();
                async move {
                    match event {
                        async_nats::Event::Connected => {
                            info!("NATS reconnected, signaling workers");
                            let _ = tx.send(());
                        }
                        async_nats::Event::Disconnected => {
                            warn!("NATS disconnected");
                        }
                        _ => {}
                    }
                }
            });

        // Add authentication if configured
        if let Some(token) = &config.token {
            connect_options = connect_options.token(token.clone());
        } else if let (Some(user), Some(pass)) = (&config.username, &config.password) {
            connect_options = connect_options.user_and_password(user.clone(), pass.clone());
        }

        info!("Connecting to NATS at {}", config.url);
        let client = connect_options.connect(&config.url).await?;
        info!("Connected to NATS");

        let jetstream = jetstream::new(client.clone());

        let nats_client = Self {
            client,
            jetstream,
            reconnect_tx: Arc::new(reconnect_tx),
        };

        // Set up required streams
        nats_client.setup_streams().await?;

        Ok(nats_client)
    }

    /// Try to connect to NATS, returning None if it fails.
    /// Useful for optional NATS integration.
    pub async fn try_connect(config: &NatsConfig) -> Option<Self> {
        match Self::connect(config).await {
            Ok(client) => Some(client),
            Err(e) => {
                warn!(
                    "Failed to connect to NATS: {}. NATS features will be disabled.",
                    e
                );
                None
            }
        }
    }

    /// Get the underlying async-nats client.
    pub fn client(&self) -> &async_nats::Client {
        &self.client
    }

    /// Get the JetStream context.
    pub fn jetstream(&self) -> &jetstream::Context {
        &self.jetstream
    }

    /// Subscribe to reconnection events.
    ///
    /// Returns a receiver that fires when the NATS connection is re-established.
    /// Workers should use this to recreate their consumers after a reconnection.
    pub fn subscribe_reconnect(&self) -> broadcast::Receiver<()> {
        self.reconnect_tx.subscribe()
    }

    /// Set up the required JetStream streams.
    async fn setup_streams(&self) -> Result<(), NatsError> {
        // JOBS stream - work queue pattern (each message processed once)
        // allow_direct enables direct_get for per-message ghost reconciliation.
        let jobs_config = StreamConfig {
            name: "JOBS".to_string(),
            subjects: vec!["jobs.>".to_string()],
            retention: RetentionPolicy::WorkQueue,
            max_age: Duration::from_secs(7 * 24 * 60 * 60), // 7 days
            allow_direct: true,
            ..Default::default()
        };

        // create_or_update_stream ensures config changes (like allow_direct)
        // propagate to existing streams on next startup.
        match self.jetstream.create_or_update_stream(jobs_config).await {
            Ok(info) => {
                info!("JOBS stream ready: {} messages", info.state.messages);
            }
            Err(e) => {
                error!("Failed to create/update JOBS stream: {}", e);
                return Err(NatsError::JetStreamContext(e));
            }
        }

        // EVENTS stream - pub/sub pattern (multiple consumers)
        let events_config = StreamConfig {
            name: "EVENTS".to_string(),
            subjects: vec!["events.>".to_string()],
            retention: RetentionPolicy::Limits,
            max_age: Duration::from_secs(24 * 60 * 60), // 24 hours
            ..Default::default()
        };

        match self.jetstream.create_or_update_stream(events_config).await {
            Ok(info) => {
                info!("EVENTS stream ready: {} messages", info.state.messages);
            }
            Err(e) => {
                error!("Failed to create/update EVENTS stream: {}", e);
                return Err(NatsError::JetStreamContext(e));
            }
        }

        Ok(())
    }

    /// Subscribe to a core NATS subject (non-JetStream).
    ///
    /// Used for advisory subjects like `$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.*.*`.
    pub async fn subscribe(
        &self,
        subject: impl Into<String>,
    ) -> Result<async_nats::Subscriber, NatsError> {
        Ok(self.client.subscribe(subject.into()).await?)
    }

    /// Check if NATS is connected.
    pub fn is_connected(&self) -> bool {
        self.client.connection_state() == async_nats::connection::State::Connected
    }
}

impl std::fmt::Debug for NatsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NatsClient")
            .field("connected", &self.is_connected())
            .finish()
    }
}
