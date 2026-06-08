//! First-class file-server entities (docs/32 §4.1).
//!
//! `file_servers` is the hybrid entity that gives `file_inventory.file_server_id`
//! an identity: a transport `kind`, an optional `resource_ref` to the workspace
//! resource holding its connection + secrets (secrets stay in Vault — never on
//! the entity), and derived rollups (file count / size / status) joined from
//! `file_inventory` by `key`. See `handlers` for the HTTP surface.

pub mod handlers;
pub mod model;
pub mod queries;
