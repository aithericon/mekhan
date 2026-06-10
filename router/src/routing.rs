//! Replica selection.
//!
//! Eligibility = a live replica that serves the requested model. Placement =
//! when the request carries a required residency zone, the chosen replica MUST
//! be in that zone — **GDPR fail-closed**: never cross-zone, never an external
//! auto-offload (doc 28 §7/§11). Among placeable replicas, pick the
//! least-loaded one (the most free admission permits) so vLLM's continuous
//! batcher on each engine stays fed without any single engine over-subscribed.
//!
//! Zone filtering here is a simple string match and stays as-is; if this
//! module ever grows capability/requirements **constraint matching**, it MUST
//! call the shared matcher `inference_core::capability::satisfies` (the
//! conformance-tested transcription of the engine's authoritative Rhai
//! `satisfies`) — do not hand-roll the relation.

use std::sync::Arc;

use tokio::sync::{RwLock, Semaphore};

use crate::config::ReplicaConfig;

/// One upstream model-server replica + its live admission semaphore.
#[derive(Debug)]
pub struct Replica {
    pub id: String,
    pub base_url: String,
    pub residency_zone: Option<String>,
    pub model_ids: Vec<String>,
    pub concurrency_c: usize,
    pub api_key: Option<String>,
    /// Sized to `concurrency_c` (vLLM `--max-num-seqs`). A permit is held for
    /// the entire request lifetime (full SSE stream).
    pub sem: Arc<Semaphore>,
    pub live: bool,
}

impl Replica {
    pub fn serves(&self, model: &str) -> bool {
        self.model_ids.iter().any(|m| m == model)
    }
}

/// A point-in-time view of a replica for `/metrics` exposition.
#[derive(Debug, Clone)]
pub struct ReplicaStat {
    pub id: String,
    pub residency_zone: Option<String>,
    pub model_ids: Vec<String>,
    pub capacity: usize,
    pub in_flight: usize,
    pub live: bool,
}

/// The in-process replica inventory. `RwLock` so the (deferred) inventory poll
/// can hot-swap it without restarting the router.
pub struct ReplicaTable {
    replicas: RwLock<Vec<Arc<Replica>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    #[error("no live replica serves model `{0}`")]
    NoReplica(String),
    #[error("no replica serves model `{model}` in residency zone `{zone}`")]
    ResidencyUnsatisfiable { model: String, zone: String },
}

impl ReplicaTable {
    pub fn from_config(replicas: &[ReplicaConfig]) -> Self {
        let built = replicas
            .iter()
            .enumerate()
            .map(|(i, r)| Arc::new(build_replica(i, r)))
            .collect();
        Self {
            replicas: RwLock::new(built),
        }
    }

    /// Select a replica for `(model, required_zone)`. See module docs for the
    /// eligibility/placement/least-loaded contract.
    pub async fn select(
        &self,
        model: &str,
        required_zone: Option<&str>,
    ) -> Result<Arc<Replica>, RouteError> {
        let guard = self.replicas.read().await;
        let serving: Vec<&Arc<Replica>> =
            guard.iter().filter(|r| r.live && r.serves(model)).collect();
        if serving.is_empty() {
            return Err(RouteError::NoReplica(model.to_string()));
        }
        let placeable: Vec<&Arc<Replica>> = match required_zone {
            Some(zone) => serving
                .into_iter()
                .filter(|r| r.residency_zone.as_deref() == Some(zone))
                .collect(),
            None => serving,
        };
        // Least-loaded = most free admission permits.
        match placeable
            .into_iter()
            .max_by_key(|r| r.sem.available_permits())
        {
            Some(r) => Ok(r.clone()),
            None => Err(RouteError::ResidencyUnsatisfiable {
                model: model.to_string(),
                zone: required_zone.unwrap_or_default().to_string(),
            }),
        }
    }

    /// Replace the whole inventory (inventory-poll seam).
    pub async fn replace(&self, replicas: Vec<Arc<Replica>>) {
        *self.replicas.write().await = replicas;
    }

    /// Distinct, sorted model ids served by live replicas (`GET /v1/models`).
    pub async fn live_model_ids(&self) -> Vec<String> {
        let guard = self.replicas.read().await;
        let mut ids: Vec<String> = guard
            .iter()
            .filter(|r| r.live)
            .flat_map(|r| r.model_ids.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Per-replica stats for `/metrics`.
    pub async fn snapshot(&self) -> Vec<ReplicaStat> {
        self.replicas
            .read()
            .await
            .iter()
            .map(|r| {
                let available = r.sem.available_permits();
                ReplicaStat {
                    id: r.id.clone(),
                    residency_zone: r.residency_zone.clone(),
                    model_ids: r.model_ids.clone(),
                    capacity: r.concurrency_c,
                    in_flight: r.concurrency_c.saturating_sub(available),
                    live: r.live,
                }
            })
            .collect()
    }
}

fn build_replica(idx: usize, cfg: &ReplicaConfig) -> Replica {
    let c = cfg.concurrency_c.max(1);
    Replica {
        id: format!("replica-{idx}"),
        base_url: cfg.base_url.trim_end_matches('/').to_string(),
        residency_zone: cfg.residency_zone.clone(),
        model_ids: cfg.model_ids.clone(),
        concurrency_c: c,
        api_key: cfg.api_key.clone(),
        sem: Arc::new(Semaphore::new(c)),
        live: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(base: &str, models: &[&str], zone: Option<&str>, c: usize) -> ReplicaConfig {
        ReplicaConfig {
            base_url: base.to_string(),
            model_ids: models.iter().map(|s| s.to_string()).collect(),
            residency_zone: zone.map(|z| z.to_string()),
            concurrency_c: c,
            api_key: None,
        }
    }

    #[tokio::test]
    async fn no_replica_for_unknown_model_is_503_signal() {
        let table = ReplicaTable::from_config(&[cfg("http://a", &["m1"], Some("eu"), 2)]);
        let err = table.select("nope", None).await.unwrap_err();
        assert!(matches!(err, RouteError::NoReplica(_)));
    }

    #[tokio::test]
    async fn residency_required_filters_strictly_and_never_crosses_zone() {
        let table = ReplicaTable::from_config(&[
            cfg("http://eu", &["m1"], Some("eu-west"), 2),
            cfg("http://us", &["m1"], Some("us-east"), 2),
        ]);
        // request eu-west → eu replica
        let r = table.select("m1", Some("eu-west")).await.unwrap();
        assert_eq!(r.base_url, "http://eu");
        // request a zone nobody serves → ResidencyUnsatisfiable, NOT a cross-zone pick
        let err = table.select("m1", Some("ap-south")).await.unwrap_err();
        assert!(matches!(err, RouteError::ResidencyUnsatisfiable { .. }));
    }

    #[tokio::test]
    async fn no_zone_constraint_allows_any_serving_replica() {
        let table = ReplicaTable::from_config(&[cfg("http://eu", &["m1"], Some("eu-west"), 2)]);
        let r = table.select("m1", None).await.unwrap();
        assert_eq!(r.base_url, "http://eu");
    }

    #[tokio::test]
    async fn picks_least_loaded_replica() {
        let table = ReplicaTable::from_config(&[
            cfg("http://a", &["m1"], None, 2),
            cfg("http://b", &["m1"], None, 2),
        ]);
        // Saturate replica-0 by holding both its permits.
        let first = table.select("m1", None).await.unwrap();
        let _p1 = first.sem.clone().try_acquire_owned().unwrap();
        let _p2 = first.sem.clone().try_acquire_owned().unwrap();
        // Now the other replica has more free permits → it is chosen.
        let next = table.select("m1", None).await.unwrap();
        assert_ne!(next.base_url, first.base_url);
    }

    #[tokio::test]
    async fn live_model_ids_dedup_sorted() {
        let table = ReplicaTable::from_config(&[
            cfg("http://a", &["m2", "m1"], None, 1),
            cfg("http://b", &["m1", "m3"], None, 1),
        ]);
        assert_eq!(table.live_model_ids().await, vec!["m1", "m2", "m3"]);
    }
}
