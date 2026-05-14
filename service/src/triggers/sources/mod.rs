//! Per-source firing logic for trigger nodes. Phase 5a only ships `manual`;
//! the other sources land in 5b–5e:
//!
//! - `manual` — fires from `POST /api/triggers/{node_id}/fire` (Phase 5a).
//! - `cron` — scheduled fires (Phase 5b).
//! - `catalog` — fires from `CatalogueEntry` ingest matching filters (Phase 5c).
//! - `net_completion` — fires from lifecycle event stream (Phase 5d).
//! - `webhook` — fires from `POST /api/triggers/webhook/{slug}` (Phase 5e).
//!
//! Each module is a thin glue layer between an event stream and
//! `TriggerDispatcher::fire`. They share the dispatcher's fire path so payload
//! mapping, concurrency policy, and history accounting all happen in one place.

pub mod catalog;
pub mod cron;
pub mod manual;
pub mod net_completion;
pub mod webhook;
