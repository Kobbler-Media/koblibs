use std::sync::Arc;
use std::time::Duration;

use crate::engine::providers::Providers;

pub async fn run(providers: Arc<Providers>, every: Duration) {
    loop {
        providers.client.warm().await;
        tokio::time::sleep(every).await;
        if let Err(error) = providers.refresh() {
            tracing::warn!(%error, "could not rebuild providers");
        }
        providers.save_usage();
        let store = Arc::clone(&providers.store);
        let swept = tokio::task::spawn_blocking(move || store.sweep()).await;
        if let Ok(Err(error)) = swept {
            tracing::warn!(%error, "could not trim the release cache");
        }
    }
}
