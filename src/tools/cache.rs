use std::collections::HashMap;
use std::time::{Duration, Instant};

use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde_json::Value;
use tokio::sync::{OnceCell, RwLock};

struct Entry {
    value: Value,
    expires_at: Instant,
}

/// Tool-result cache. Memory is always available; Redis is an optional shared
/// layer for multiple app instances. If Redis is down, calls silently use memory.
pub struct Cache {
    inner: RwLock<HashMap<String, Entry>>,
    redis_url: Option<String>,
    redis: OnceCell<Option<ConnectionManager>>,
}

impl Cache {
    pub fn new(redis_url: Option<String>) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            redis_url,
            redis: OnceCell::new(),
        }
    }

    async fn redis(&self) -> Option<ConnectionManager> {
        self.redis
            .get_or_init(|| async {
                let url = self.redis_url.as_deref()?;
                let client = match redis::Client::open(url) {
                    Ok(client) => client,
                    Err(error) => {
                        tracing::warn!(%error, "invalid REDIS_URL; using in-memory cache");
                        return None;
                    }
                };
                match tokio::time::timeout(
                    Duration::from_millis(800),
                    ConnectionManager::new(client),
                )
                .await
                {
                    Ok(Ok(manager)) => {
                        tracing::info!("Redis tool cache connected");
                        Some(manager)
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "Redis is unavailable; using in-memory cache");
                        None
                    }
                    Err(_) => {
                        tracing::warn!("Redis connection timed out; using in-memory cache");
                        None
                    }
                }
            })
            .await
            .clone()
    }

    fn redis_key(key: &str) -> String {
        format!("travel-ai:tool:{key}")
    }

    pub async fn get(&self, key: &str) -> Option<Value> {
        if let Some(mut connection) = self.redis().await {
            let result = tokio::time::timeout(
                Duration::from_millis(500),
                connection.get::<_, Option<String>>(Self::redis_key(key)),
            )
            .await;
            if let Ok(Ok(Some(raw))) = result {
                if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                    return Some(value);
                }
            }
        }

        let now = Instant::now();
        let guard = self.inner.read().await;
        guard
            .get(key)
            .filter(|entry| entry.expires_at > now)
            .map(|entry| entry.value.clone())
    }

    pub async fn put(&self, key: String, value: Value, ttl: Duration) {
        let entry = Entry {
            value: value.clone(),
            expires_at: Instant::now() + ttl,
        };
        let now = Instant::now();
        {
            let mut guard = self.inner.write().await;
            guard.retain(|_, entry| entry.expires_at > now);
            while guard.len() >= 512 {
                let Some(oldest) = guard.keys().next().cloned() else {
                    break;
                };
                guard.remove(&oldest);
            }
            guard.insert(key.clone(), entry);
        }

        if let Some(mut connection) = self.redis().await {
            let seconds = ttl.as_secs().max(1);
            if let Ok(raw) = serde_json::to_string(&value) {
                let _ = tokio::time::timeout(
                    Duration::from_millis(500),
                    connection.set_ex::<_, _, ()>(Self::redis_key(&key), raw, seconds),
                )
                .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_cache_works_without_redis() {
        let cache = Cache::new(None);
        cache
            .put(
                "k".to_string(),
                serde_json::json!({"ok": true}),
                Duration::from_secs(60),
            )
            .await;
        assert_eq!(cache.get("k").await.unwrap()["ok"], true);
    }
}
