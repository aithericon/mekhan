//! Petri meta tag constants and routing metadata.
//!
//! Scheduler clients embed these tags in job metadata when submitting jobs.
//! Scheduler watchers extract them to route signals to the correct net and place.

use std::collections::HashMap;

/// Meta key for the Petri net ID.
pub const META_NET_ID: &str = "petri_net_id";

/// Meta key for the default signal place name.
pub const META_PLACE: &str = "petri_place";

/// Meta key for the signal key (matches signals to tokens).
pub const META_SIGNAL_KEY: &str = "petri_signal_key";

/// Prefix for per-status signal routing meta keys.
///
/// Full key: `petri_signal_{status}` where status is the snake_case `JobStatus` name.
/// Example: `petri_signal_running`, `petri_signal_completed`.
pub const META_SIGNAL_PREFIX: &str = "petri_signal_";

/// Prefix for per-category event routing meta keys.
///
/// Full key: `petri_event_{category}` where category is the snake_case event category.
/// Example: `petri_event_progress`, `petri_event_artifact`.
/// Used by the executor integration for mid-execution event routing.
pub const META_EVENT_PREFIX: &str = "petri_event_";

/// Build a meta key for a specific status route.
///
/// Example: `signal_meta_key("running")` -> `"petri_signal_running"`
pub fn signal_meta_key(status: &str) -> String {
    format!("{}{}", META_SIGNAL_PREFIX, status)
}

/// Extract the status name from a signal routing meta key.
///
/// Returns `None` if the key does not start with the prefix.
pub fn parse_signal_meta_key(key: &str) -> Option<&str> {
    key.strip_prefix(META_SIGNAL_PREFIX)
}

/// Build a meta key for a specific event category route.
///
/// Example: `event_meta_key("progress")` -> `"petri_event_progress"`
pub fn event_meta_key(category: &str) -> String {
    format!("{}{}", META_EVENT_PREFIX, category)
}

/// Extract the event category name from an event routing meta key.
///
/// Returns `None` if the key does not start with the event prefix.
pub fn parse_event_meta_key(key: &str) -> Option<&str> {
    key.strip_prefix(META_EVENT_PREFIX)
}

/// Routing metadata extracted from scheduler job tags.
///
/// Contains everything a watcher needs to route a status signal to the
/// correct Petri net and place.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoutingMeta {
    /// The Petri net ID this job belongs to.
    pub net_id: String,
    /// Default place for signals when no per-status route matches.
    pub fallback_place: String,
    /// Per-status signal routes: `status_name` -> `place_name`.
    ///
    /// Example: `"running" -> "running_inbox"` routes `Running` signals
    /// to a different place than the fallback.
    pub signal_routes: HashMap<String, String>,
    /// Per-category event routes: `category` -> `place_name`.
    ///
    /// Used by executor integration for mid-execution events (progress, artifact, etc.).
    /// If a category has no route, the event is not published to the net.
    #[serde(default)]
    pub event_routes: HashMap<String, String>,
    /// Signal key for matching signals to tokens.
    pub signal_key: String,
}

impl RoutingMeta {
    /// Resolve the target place for a given job status.
    ///
    /// Checks `signal_routes` first; if no entry exists for this status,
    /// falls back to `fallback_place`.
    pub fn place_for_status(&self, status: &str) -> &str {
        self.signal_routes
            .get(status)
            .map(|s| s.as_str())
            .unwrap_or(&self.fallback_place)
    }

    /// Resolve the target place for a mid-execution event category.
    ///
    /// Returns `None` if no route is configured for this category,
    /// meaning the event should not be published to the net.
    pub fn place_for_event(&self, category: &str) -> Option<&str> {
        self.event_routes.get(category).map(|s| s.as_str())
    }

    /// Extract routing metadata from a scheduler job's metadata tags.
    ///
    /// Returns `None` if required keys (`petri_net_id`, `petri_place`) are missing.
    pub fn from_meta_tags(meta: &HashMap<String, String>) -> Option<Self> {
        let net_id = meta.get(META_NET_ID)?.clone();
        let fallback_place = meta.get(META_PLACE)?.clone();
        let signal_key = meta.get(META_SIGNAL_KEY).cloned().unwrap_or_default();

        let mut signal_routes = HashMap::new();
        let mut event_routes = HashMap::new();
        for (key, value) in meta {
            // Skip META_SIGNAL_KEY — it shares the prefix but is not a status route.
            if key == META_SIGNAL_KEY {
                continue;
            }
            if let Some(status) = parse_signal_meta_key(key) {
                signal_routes.insert(status.to_string(), value.clone());
            } else if let Some(category) = parse_event_meta_key(key) {
                event_routes.insert(category.to_string(), value.clone());
            }
        }

        Some(Self {
            net_id,
            fallback_place,
            signal_routes,
            event_routes,
            signal_key,
        })
    }

    /// Build the set of meta tags for stamping into a scheduler job.
    ///
    /// Used by `SchedulerClient` / `ExecutorClient` implementations when submitting jobs.
    pub fn to_meta_tags(&self) -> HashMap<String, String> {
        let mut meta = HashMap::new();
        meta.insert(META_NET_ID.to_string(), self.net_id.clone());
        meta.insert(META_PLACE.to_string(), self.fallback_place.clone());
        if !self.signal_key.is_empty() {
            meta.insert(META_SIGNAL_KEY.to_string(), self.signal_key.clone());
        }
        for (status, place) in &self.signal_routes {
            meta.insert(signal_meta_key(status), place.clone());
        }
        for (category, place) in &self.event_routes {
            meta.insert(event_meta_key(category), place.clone());
        }
        meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_meta_key() {
        assert_eq!(signal_meta_key("running"), "petri_signal_running");
        assert_eq!(signal_meta_key("completed"), "petri_signal_completed");
    }

    #[test]
    fn test_parse_signal_meta_key() {
        assert_eq!(
            parse_signal_meta_key("petri_signal_running"),
            Some("running")
        );
        assert_eq!(parse_signal_meta_key("petri_net_id"), None);
        assert_eq!(parse_signal_meta_key("unrelated"), None);
    }

    #[test]
    fn test_event_meta_key() {
        assert_eq!(event_meta_key("progress"), "petri_event_progress");
        assert_eq!(event_meta_key("artifact"), "petri_event_artifact");
    }

    #[test]
    fn test_parse_event_meta_key() {
        assert_eq!(
            parse_event_meta_key("petri_event_progress"),
            Some("progress")
        );
        assert_eq!(parse_event_meta_key("petri_signal_running"), None);
        assert_eq!(parse_event_meta_key("unrelated"), None);
    }

    #[test]
    fn test_place_for_status_fallback() {
        let meta = RoutingMeta {
            net_id: "gpu-resource".into(),
            fallback_place: "status_inbox".into(),
            signal_routes: HashMap::new(),
            event_routes: HashMap::new(),
            signal_key: "train-alpha:0".into(),
        };

        assert_eq!(meta.place_for_status("running"), "status_inbox");
        assert_eq!(meta.place_for_status("completed"), "status_inbox");
        assert_eq!(meta.place_for_status("failed"), "status_inbox");
    }

    #[test]
    fn test_place_for_status_with_routes() {
        let mut signal_routes = HashMap::new();
        signal_routes.insert("running".into(), "running_inbox".into());
        signal_routes.insert("completed".into(), "done_inbox".into());

        let meta = RoutingMeta {
            net_id: "gpu-resource".into(),
            fallback_place: "status_inbox".into(),
            signal_routes,
            event_routes: HashMap::new(),
            signal_key: "train-alpha:0".into(),
        };

        assert_eq!(meta.place_for_status("running"), "running_inbox");
        assert_eq!(meta.place_for_status("completed"), "done_inbox");
        assert_eq!(meta.place_for_status("failed"), "status_inbox");
        assert_eq!(meta.place_for_status("queued"), "status_inbox");
    }

    #[test]
    fn test_place_for_event() {
        let mut event_routes = HashMap::new();
        event_routes.insert("progress".into(), "sig_progress".into());
        event_routes.insert("artifact".into(), "sig_artifact".into());

        let meta = RoutingMeta {
            net_id: "exec-net".into(),
            fallback_place: "inbox".into(),
            signal_routes: HashMap::new(),
            event_routes,
            signal_key: "job:0".into(),
        };

        assert_eq!(meta.place_for_event("progress"), Some("sig_progress"));
        assert_eq!(meta.place_for_event("artifact"), Some("sig_artifact"));
        assert_eq!(meta.place_for_event("log"), None);
    }

    #[test]
    fn test_from_meta_tags() {
        let mut tags = HashMap::new();
        tags.insert("petri_net_id".into(), "my-net".into());
        tags.insert("petri_place".into(), "inbox".into());
        tags.insert("petri_signal_key".into(), "job-1:0".into());
        tags.insert("petri_signal_running".into(), "running_place".into());
        tags.insert("petri_signal_completed".into(), "done_place".into());
        tags.insert("unrelated_key".into(), "ignored".into());

        let meta = RoutingMeta::from_meta_tags(&tags).unwrap();
        assert_eq!(meta.net_id, "my-net");
        assert_eq!(meta.fallback_place, "inbox");
        assert_eq!(meta.signal_key, "job-1:0");
        assert_eq!(meta.signal_routes.len(), 2);
        assert_eq!(meta.place_for_status("running"), "running_place");
        assert_eq!(meta.place_for_status("completed"), "done_place");
        assert_eq!(meta.place_for_status("failed"), "inbox");
    }

    #[test]
    fn test_from_meta_tags_missing_required() {
        let mut tags = HashMap::new();
        tags.insert("petri_net_id".into(), "my-net".into());
        // Missing petri_place
        assert!(RoutingMeta::from_meta_tags(&tags).is_none());

        let tags2 = HashMap::new();
        // Missing everything
        assert!(RoutingMeta::from_meta_tags(&tags2).is_none());
    }

    #[test]
    fn test_to_meta_tags_roundtrip() {
        let mut signal_routes = HashMap::new();
        signal_routes.insert("running".into(), "running_inbox".into());

        let meta = RoutingMeta {
            net_id: "test-net".into(),
            fallback_place: "inbox".into(),
            signal_routes,
            event_routes: HashMap::new(),
            signal_key: "job:0".into(),
        };

        let tags = meta.to_meta_tags();
        let restored = RoutingMeta::from_meta_tags(&tags).unwrap();

        assert_eq!(restored.net_id, "test-net");
        assert_eq!(restored.fallback_place, "inbox");
        assert_eq!(restored.signal_key, "job:0");
        assert_eq!(restored.place_for_status("running"), "running_inbox");
    }

    #[test]
    fn test_to_meta_tags_roundtrip_with_events() {
        let mut signal_routes = HashMap::new();
        signal_routes.insert("running".into(), "sig_running".into());
        let mut event_routes = HashMap::new();
        event_routes.insert("progress".into(), "sig_progress".into());
        event_routes.insert("artifact".into(), "sig_artifact".into());

        let meta = RoutingMeta {
            net_id: "exec-net".into(),
            fallback_place: "inbox".into(),
            signal_routes,
            event_routes,
            signal_key: "job:0".into(),
        };

        let tags = meta.to_meta_tags();
        assert!(tags.contains_key("petri_event_progress"));
        assert!(tags.contains_key("petri_event_artifact"));

        let restored = RoutingMeta::from_meta_tags(&tags).unwrap();
        assert_eq!(restored.place_for_event("progress"), Some("sig_progress"));
        assert_eq!(restored.place_for_event("artifact"), Some("sig_artifact"));
        assert_eq!(restored.place_for_event("log"), None);
        assert_eq!(restored.place_for_status("running"), "sig_running");
    }

    #[test]
    fn test_from_meta_tags_with_events() {
        let mut tags = HashMap::new();
        tags.insert("petri_net_id".into(), "my-net".into());
        tags.insert("petri_place".into(), "inbox".into());
        tags.insert("petri_signal_running".into(), "sig_running".into());
        tags.insert("petri_event_progress".into(), "sig_progress".into());

        let meta = RoutingMeta::from_meta_tags(&tags).unwrap();
        assert_eq!(meta.signal_routes.len(), 1);
        assert_eq!(meta.event_routes.len(), 1);
        assert_eq!(meta.place_for_status("running"), "sig_running");
        assert_eq!(meta.place_for_event("progress"), Some("sig_progress"));
    }
}
