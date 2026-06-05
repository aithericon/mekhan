//! Replica inventory.
//!
//! The MVP inventory is the **static** table built from `ROUTER_REPLICAS` /
//! the config file (`routing::ReplicaTable::from_config`). The live poll of
//! mekhan's `GET /api/v1/capacities` + fleet snapshot + runner interface
//! catalog "served models" (doc 11 §5.2, doc 29 Router-MVP) — which would
//! prune offline replicas and pick up newly-loaded models without a restart —
//! is the soft-dep upgrade deferred to doc 11 P2. The seam is wired here so
//! it's explicit: when `mekhan_url` is set we log that the poll is pending;
//! the table hot-swap path (`ReplicaTable::replace`) is already in place.

use std::sync::Arc;

use tracing::info;

use crate::routing::ReplicaTable;

/// Start the inventory refresher. MVP: logs the chosen mode and returns —
/// the static table from config is authoritative.
pub fn spawn_inventory_refresh(_table: Arc<ReplicaTable>, mekhan_url: Option<String>) {
    match mekhan_url {
        Some(url) => info!(
            %url,
            "inventory: live mekhan poll seam present but deferred (doc 11 P2); \
             using the static replica table"
        ),
        None => info!("inventory: static replica table (no mekhan_url configured)"),
    }
}
