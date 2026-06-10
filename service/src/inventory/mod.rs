//! File inventory — one row per physical copy of a file on a file server
//! (docs/32). Populated by `crawl` (online) via the register API; logically
//! linked to the content-addressed catalogue by `content_hash`.

pub mod fold;
pub mod handlers;
pub mod model;
pub mod queries;
pub mod reconcile;
pub mod repository;
