//! Replica inventory.
//!
//! The static table built from `ROUTER_REPLICAS` / the config file
//! (`routing::ReplicaTable::from_config`) is the cold-start authority. When
//! `mekhan_url` is configured, the LIVE poll below (doc 11 §5.2, docs/29 GAP A)
//! hot-swaps it every 30s off mekhan's public model-serving aggregator
//! (`GET /api/v1/runners/model-serving`) so routing reflects what runners
//! actually serve — newly-loaded models appear, drained nodes drop out — without
//! a router restart. The table hot-swap path (`ReplicaTable::replace`) is the
//! seam.
//!
//! Fail-soft: a transport error, a non-2xx, or an empty inventory LEAVES the
//! prior/static table in place (a momentary mekhan blip must not black-hole
//! routing). The router does NOT path-dep mekhan — [`ServingRunner`] is a local
//! mirror of mekhan's `ModelServingRunner` wire shape (the EXACT field names).

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::routing::{Replica, ReplicaTable};

/// One live model-serving runner, as mekhan's public aggregator emits it. A
/// LOCAL mirror of `mekhan_service::models::runner::ModelServingRunner` — the
/// router must not path-dep mekhan, so the contract is the shared field names.
#[derive(Debug, Clone, Deserialize)]
struct ServingRunner {
    runner_id: String,
    base_url: String,
    #[serde(default)]
    residency_zone: Option<String>,
    #[serde(default)]
    model_ids: Vec<String>,
    concurrency_c: usize,
}

/// Poll interval for the live inventory refresh.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Start the inventory refresher.
///
/// With `mekhan_url` set: spawns a 30s poll loop that replaces the replica table
/// off mekhan's live inventory (keeping the prior table on any failure). Without
/// it: the static config table is authoritative and we return immediately.
pub fn spawn_inventory_refresh(table: Arc<ReplicaTable>, mekhan_url: Option<String>) {
    let Some(base) = mekhan_url else {
        info!("inventory: static replica table (no mekhan_url configured)");
        return;
    };
    let url = format!(
        "{}/api/v1/runners/model-serving",
        base.trim_end_matches('/')
    );
    info!(%url, "inventory: live mekhan poll every {}s", POLL_INTERVAL.as_secs());

    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        loop {
            ticker.tick().await;
            match fetch_inventory(&client, &url).await {
                Ok(servers) if servers.is_empty() => {
                    warn!(%url, "inventory: mekhan returned an empty list; keeping prior table");
                }
                Ok(servers) => {
                    let replicas = servers_to_replicas(servers);
                    let n = replicas.len();
                    table.replace(replicas).await;
                    debug!(%url, replicas = n, "inventory: replaced replica table from live poll");
                }
                Err(e) => {
                    warn!(%url, error = %e, "inventory: poll failed; keeping prior table");
                }
            }
        }
    });
}

/// GET + deserialize mekhan's inventory. A non-2xx is an error (so the caller
/// keeps the prior table).
async fn fetch_inventory(
    client: &reqwest::Client,
    url: &str,
) -> Result<Vec<ServingRunner>, reqwest::Error> {
    client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<ServingRunner>>()
        .await
}

/// PURE map from mekhan's live serving runners to router replicas. The admission
/// semaphore is sized to `concurrency_c` with a floor of 1 (a zero budget would
/// permanently wedge the engine); `live = true` (mekhan only emits PRESENT
/// runners); no per-replica `api_key` (in-cluster runners are unauthenticated to
/// the router). The id is `runner-{runner_id}` so router logs/metrics tie back to
/// the fleet row.
fn servers_to_replicas(servers: Vec<ServingRunner>) -> Vec<Arc<Replica>> {
    servers
        .into_iter()
        .map(|s| {
            let c = s.concurrency_c.max(1);
            Arc::new(Replica {
                id: format!("runner-{}", s.runner_id),
                base_url: s.base_url.trim_end_matches('/').to_string(),
                residency_zone: s.residency_zone,
                model_ids: s.model_ids,
                concurrency_c: c,
                api_key: None,
                sem: Arc::new(Semaphore::new(c)),
                live: true,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(id: &str, models: &[&str], zone: Option<&str>, c: usize) -> ServingRunner {
        ServingRunner {
            runner_id: id.to_string(),
            base_url: "http://node/".to_string(),
            residency_zone: zone.map(|z| z.to_string()),
            model_ids: models.iter().map(|m| m.to_string()).collect(),
            concurrency_c: c,
        }
    }

    #[test]
    fn replica_sizes_semaphore_and_carries_zone_and_models() {
        let replicas = servers_to_replicas(vec![srv(
            "abc",
            &["llama3", "my-lora"],
            Some("eu-dev"),
            4,
        )]);
        assert_eq!(replicas.len(), 1);
        let r = &replicas[0];
        assert_eq!(r.id, "runner-abc");
        // base_url is trailing-slash trimmed.
        assert_eq!(r.base_url, "http://node");
        assert_eq!(r.residency_zone.as_deref(), Some("eu-dev"));
        assert_eq!(r.model_ids, vec!["llama3", "my-lora"]);
        assert_eq!(r.concurrency_c, 4);
        // The admission semaphore is sized to concurrency_c.
        assert_eq!(r.sem.available_permits(), 4);
        assert!(r.live);
        assert!(r.api_key.is_none());
    }

    #[test]
    fn zero_concurrency_is_floored_to_one() {
        // A zero budget would permanently wedge the engine — the min-1 guard
        // keeps it routable.
        let replicas = servers_to_replicas(vec![srv("z", &["m1"], None, 0)]);
        assert_eq!(replicas[0].concurrency_c, 1);
        assert_eq!(replicas[0].sem.available_permits(), 1);
        assert!(replicas[0].residency_zone.is_none());
    }

    #[test]
    fn maps_every_server_to_a_replica() {
        let replicas = servers_to_replicas(vec![
            srv("a", &["m1"], None, 2),
            srv("b", &["m2"], Some("us-east"), 1),
        ]);
        assert_eq!(replicas.len(), 2);
        assert_eq!(replicas[0].id, "runner-a");
        assert_eq!(replicas[1].id, "runner-b");
        assert_eq!(replicas[1].residency_zone.as_deref(), Some("us-east"));
    }

    #[test]
    fn deserializes_mekhan_wire_shape() {
        // The exact ModelServingRunner JSON mekhan emits must deserialize, incl.
        // an omitted residency_zone (serde default).
        let json = serde_json::json!([
            {
                "runner_id": "f1d2",
                "base_url": "http://node:8000",
                "model_ids": ["llama3"],
                "concurrency_c": 4
            }
        ]);
        let servers: Vec<ServingRunner> = serde_json::from_value(json).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].runner_id, "f1d2");
        assert!(servers[0].residency_zone.is_none());
        assert_eq!(servers[0].concurrency_c, 4);
    }
}
