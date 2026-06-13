//! Starfish-style file analytics (docs/32, Cuts 1+2).
//!
//! Aggregation reads over `file_inventory`'s promoted columns (migration
//! 20240166: `size_bytes`/`mtime`/`uid`/`gid` + GENERATED `extension`) plus
//! growth snapshots over the `inventory_snapshots` hypertable (migration
//! 20240167). Three endpoints under `/api/v1/data/analytics/*`:
//!
//! * `GET  /breakdown`  — generic group-by (server / extension / size_class /
//!   age / mtime_age / owner / directory) scoped by the inventory filter DSL.
//!   The `directory` dimension doubles as the treemap level loader (`under` +
//!   `depth` lazy descent).
//! * `GET  /timeseries` — deduped growth points over `inventory_snapshots`.
//! * `POST /snapshot`   — manual trigger for the same writer the background
//!   job uses ([`snapshot::write_snapshot`]).

pub mod handlers;
pub mod model;
pub mod queries;
pub mod snapshot;
/// Per-template usage analytics (`/api/v1/templates/{id}/analytics*`) — the
/// summary + timeseries read surface over the `template_*_rollup` tables, plus
/// the one-time backfill that seeds them from the durable source tables.
pub mod template;
