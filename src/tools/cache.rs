use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::RwLock;

#[derive(Debug)]
struct Entry {
    value: Value,
    expires_at: Instant,
}

#[derive(Debug, Default)]
pub struct Cache {
    inner: RwLock<HashMap<String, Entry>>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, key: &str) -> Option<Value> {
        let now = Instant::now();
        let guard = self.inner.read().await;
        guard
            .get(key)
            .filter(|entry| entry.expires_at > now)
            .map(|entry| entry.value.clone())
    }

    pub async fn put(&self, key: String, value: Value, ttl: Duration) {
        let entry = Entry {
            value,
            expires_at: Instant::now() + ttl,
        };
        self.inner.write().await.insert(key, entry);
    }
}
