//! Idempotency cache for token commands.
//!
//! This module provides deduplication for NATS token commands,
//! preventing duplicate token operations when NATS retries messages.
//!
//! The in-memory map is optionally backed by a JetStream KV bucket so dedup
//! state survives engine restarts (the JetStream `Nats-Msg-Id` duplicate
//! window is only 120s — far shorter than the 1h TTL here).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Configuration for the idempotency cache.
#[derive(Debug, Clone)]
pub struct IdempotencyCacheConfig {
    /// Time-to-live for cache entries.
    pub ttl: Duration,
    /// Maximum number of entries before cleanup.
    pub max_entries: usize,
}

impl Default for IdempotencyCacheConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(3600), // 1 hour
            max_entries: 10000,
        }
    }
}

/// Cached result of a token operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CachedResult {
    /// Operation succeeded
    Success {
        /// Event sequence number
        event_sequence: u64,
        /// Token ID if applicable
        token_id: Option<String>,
    },
    /// Operation failed
    Failure {
        /// Error message
        error: String,
    },
}

/// A single cache entry.
struct CacheEntry {
    result: CachedResult,
    inserted_at: Instant,
}

/// Idempotency cache for deduplicating token commands.
///
/// Uses a simple in-memory HashMap with TTL-based expiration,
/// thread-safe via parking_lot::RwLock. With a KV store attached
/// ([`Self::with_kv`] / [`Self::durable`]), inserts are written through
/// (best-effort) and memory misses fall back to a KV lookup, so dedup
/// survives engine restarts. KV entry expiry is the bucket `max_age`.
pub struct IdempotencyCache {
    entries: RwLock<HashMap<String, CacheEntry>>,
    config: IdempotencyCacheConfig,
    kv: Option<async_nats::jetstream::kv::Store>,
}

impl IdempotencyCache {
    /// Base name for the JetStream KV bucket backing durable caches.
    ///
    /// The live bucket is per-workspace: `petri-idempotency-{ws}`, built via
    /// [`crate::kv_bucket_for`]. Each workspace gets an isolated dedup window so
    /// one tenant's token-command retries can never collide with another's.
    pub const KV_BUCKET: &'static str = "petri-idempotency";

    /// Create a new idempotency cache with default configuration.
    pub fn new() -> Self {
        Self::with_config(IdempotencyCacheConfig::default())
    }

    /// Create a new idempotency cache with custom configuration.
    pub fn with_config(config: IdempotencyCacheConfig) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            config,
            kv: None,
        }
    }

    /// Create a cache backed by an existing KV store.
    pub fn with_kv(config: IdempotencyCacheConfig, kv: async_nats::jetstream::kv::Store) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            config,
            kv: Some(kv),
        }
    }

    /// Create a durable cache scoped to `workspace_id`: default config, backed
    /// by the per-workspace `petri-idempotency-{ws}` bucket (created if missing,
    /// `max_age` = TTL).
    pub async fn durable(
        jetstream: &async_nats::jetstream::Context,
        workspace_id: &str,
    ) -> Result<Self, String> {
        let config = IdempotencyCacheConfig::default();
        let kv = Self::ensure_kv_bucket(jetstream, config.ttl, workspace_id).await?;
        Ok(Self::with_kv(config, kv))
    }

    /// Create or get the per-workspace idempotency KV bucket
    /// (`petri-idempotency-{ws}`) with `max_age` = `ttl`.
    pub async fn ensure_kv_bucket(
        jetstream: &async_nats::jetstream::Context,
        ttl: Duration,
        workspace_id: &str,
    ) -> Result<async_nats::jetstream::kv::Store, String> {
        let bucket = crate::kv_bucket_for(Self::KV_BUCKET, workspace_id);
        match jetstream
            .create_key_value(async_nats::jetstream::kv::Config {
                bucket: bucket.clone(),
                max_age: ttl,
                history: 1,
                ..Default::default()
            })
            .await
        {
            Ok(kv) => Ok(kv),
            // Bucket may already exist (possibly with a different config) — reuse it.
            Err(_) => jetstream
                .get_key_value(&bucket)
                .await
                .map_err(|e| format!("get idempotency KV bucket {}: {}", bucket, e)),
        }
    }

    /// Check if a key exists and is not expired.
    ///
    /// Checks memory first; on miss, falls back to the KV store (if attached)
    /// and repopulates memory on a hit.
    pub async fn get(&self, key: &str) -> Option<CachedResult> {
        {
            let entries = self.entries.read();
            if let Some(entry) = entries.get(key) {
                if entry.inserted_at.elapsed() < self.config.ttl {
                    return Some(entry.result.clone());
                }
            }
        }

        // Memory miss — try the durable store (entries expire via bucket max_age)
        let kv = self.kv.as_ref()?;
        match kv.get(kv_key(key)).await {
            Ok(Some(bytes)) => match serde_json::from_slice::<CachedResult>(&bytes) {
                Ok(result) => {
                    self.insert_memory(key.to_string(), result.clone());
                    Some(result)
                }
                Err(e) => {
                    tracing::warn!(key = %key, error = %e, "Corrupt idempotency KV entry, ignoring");
                    None
                }
            },
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(key = %key, error = %e, "Idempotency KV lookup failed");
                None
            }
        }
    }

    /// Insert a result into the cache.
    ///
    /// Writes through to the KV store when attached (best-effort: a KV
    /// failure is logged but never fails the request). Automatically cleans
    /// up expired memory entries if the cache is too large.
    pub async fn insert(&self, key: String, result: CachedResult) {
        self.insert_memory(key.clone(), result.clone());

        if let Some(kv) = &self.kv {
            match serde_json::to_vec(&result) {
                Ok(bytes) => {
                    if let Err(e) = kv.put(kv_key(&key), bytes.into()).await {
                        tracing::warn!(key = %key, error = %e, "Idempotency KV write-through failed");
                    }
                }
                Err(e) => {
                    tracing::warn!(key = %key, error = %e, "Failed to serialize idempotency entry");
                }
            }
        }
    }

    fn insert_memory(&self, key: String, result: CachedResult) {
        let mut entries = self.entries.write();

        // Cleanup if too many entries
        if entries.len() >= self.config.max_entries {
            let ttl = self.config.ttl;
            entries.retain(|_, entry| entry.inserted_at.elapsed() < ttl);
        }

        entries.insert(
            key,
            CacheEntry {
                result,
                inserted_at: Instant::now(),
            },
        );
    }

    /// Get the current number of entries (for monitoring).
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }
}

impl Default for IdempotencyCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Sanitize a cache key for NATS KV (allowed: `[-/_=.a-zA-Z0-9]`).
/// Keys are `{stream}:{sequence}` — the `:` must be replaced.
fn kv_key(key: &str) -> String {
    key.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '/' | '_' | '=' | '.' => c,
            _ => '_',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_insert_and_get() {
        let cache = IdempotencyCache::new();
        cache
            .insert(
                "key1".to_string(),
                CachedResult::Success {
                    event_sequence: 42,
                    token_id: Some("token-123".to_string()),
                },
            )
            .await;

        let result = cache.get("key1").await;
        assert!(result.is_some());
        if let Some(CachedResult::Success {
            event_sequence,
            token_id,
        }) = result
        {
            assert_eq!(event_sequence, 42);
            assert_eq!(token_id, Some("token-123".to_string()));
        } else {
            panic!("Expected Success result");
        }
    }

    #[tokio::test]
    async fn test_missing_key() {
        let cache = IdempotencyCache::new();
        assert!(cache.get("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn test_expired_entry() {
        let config = IdempotencyCacheConfig {
            ttl: Duration::from_millis(1),
            max_entries: 100,
        };
        let cache = IdempotencyCache::with_config(config);
        cache
            .insert(
                "key1".to_string(),
                CachedResult::Failure {
                    error: "test".to_string(),
                },
            )
            .await;

        // Wait for expiration
        tokio::time::sleep(Duration::from_millis(10)).await;

        assert!(cache.get("key1").await.is_none());
    }

    #[test]
    fn test_kv_key_sanitization() {
        assert_eq!(kv_key("PETRI_GLOBAL:42"), "PETRI_GLOBAL_42");
        assert_eq!(kv_key("already-valid_key.1"), "already-valid_key.1");
    }
}
