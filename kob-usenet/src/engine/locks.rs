use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::OwnedMutexGuard;

#[derive(Default)]
pub struct KeyedLocks {
    held: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl KeyedLocks {
    pub async fn lock(&self, key: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            held.retain(|_, lock| Arc::strong_count(lock) > 1);
            Arc::clone(held.entry(key.to_owned()).or_default())
        };
        lock.lock_owned().await
    }
}
