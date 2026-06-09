//! First-class file-server entities (docs/32 §4.1).
//!
//! `file_servers` is the identity-only parent that gives
//! `file_inventory.file_server_id` an identity (by `key`, soft join). The *ways
//! to reach* a backend are N child `file_server_endpoints` (object_store / s3 /
//! sftp / local_mount), each with its own `root` prefix, optional `resource_ref`
//! to the workspace resource holding connection + secrets (secrets stay in Vault
//! — never on the entity), `group_id` for local_mount dispatch, and its own
//! status / verification lifecycle. Derived rollups (file count / size / status)
//! are joined from `file_inventory` by `key`. See `handlers` for the HTTP surface.

pub mod handlers;
pub mod model;
pub mod queries;
