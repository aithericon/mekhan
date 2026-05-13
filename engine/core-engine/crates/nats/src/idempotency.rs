//! Idempotency cache for token commands.
//!
//! This module provides deduplication for NATS token commands,
//! preventing duplicate token operations when NATS retries messages.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

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
#[derive(Debug, Clone)]
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
/// Uses a simple in-memory HashMap with TTL-based expiration.
/// Thread-safe via parking_lot::RwLock.
pub struct IdempotencyCache {
    entries: RwLock<HashMap<String, CacheEntry>>,
    config: IdempotencyCacheConfig,
}

impl IdempotencyCache {
    /// Create a new idempotency cache with default configuration.
    pub fn new() -> Self {
        Self::with_config(IdempotencyCacheConfig::default())
    }

    /// Create a new idempotency cache with custom configuration.
    pub fn with_config(config: IdempotencyCacheConfig) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            config,
        }
    }

    /// Check if a key exists and is not expired.
    ///
    /// Returns the cached result if found and not expired.
    pub fn get(&self, key: &str) -> Option<CachedResult> {
        let entries = self.entries.read();
        if let Some(entry) = entries.get(key) {
            if entry.inserted_at.elapsed() < self.config.ttl {
                return Some(entry.result.clone());
            }
        }
        None
    }

    /// Insert a result into the cache.
    ///
    /// Automatically cleans up expired entries if the cache is too large.
    pub fn insert(&self, key: String, result: CachedResult) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_get() {
        let cache = IdempotencyCache::new();
        cache.insert(
            "key1".to_string(),
            CachedResult::Success {
                event_sequence: 42,
                token_id: Some("token-123".to_string()),
            },
        );

        let result = cache.get("key1");
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

    #[test]
    fn test_missing_key() {
        let cache = IdempotencyCache::new();
        assert!(cache.get("nonexistent").is_none());
    }

    #[test]
    fn test_expired_entry() {
        let config = IdempotencyCacheConfig {
            ttl: Duration::from_millis(1),
            max_entries: 100,
        };
        let cache = IdempotencyCache::with_config(config);
        cache.insert(
            "key1".to_string(),
            CachedResult::Failure {
                error: "test".to_string(),
            },
        );

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(10));

        assert!(cache.get("key1").is_none());
    }
}
