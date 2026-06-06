//! Re-export of wire-format file-ops config types from the shared
//! backend-configs crate.
//!
//! Types live in `aithericon-executor-backend-configs::file_ops` so the
//! mekhan compiler and the executor share a single source of truth for the
//! JSON shape that crosses the wire.

pub use aithericon_executor_backend_configs::file_ops::{
    AnnotateConfig, Compression, CopyConfig, CrawlConfig, DeleteConfig, FileOpsConfig, ListConfig,
    MoveConfig, ProbeConfig, StatConfig,
};
