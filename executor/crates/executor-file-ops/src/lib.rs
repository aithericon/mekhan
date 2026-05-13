//! Stateless file-operations backend for the Aithericon executor.
//!
//! Provides seven operations over any OpenDAL-supported storage backend:
//! **stat**, **copy**, **move**, **delete**, **list**, **annotate**, and **probe**.
//!
//! Every operation config carries its own inline
//! [`StorageConfig`](aithericon_executor_storage::StorageConfig) — there is no
//! default operator. Operators are built on-the-fly from these configs, which
//! means a single job can read from S3 and write to GCS.
//!
//! Copy and move use constant-memory streaming via OpenDAL's
//! `Reader`/`Writer` APIs, with optional gzip/zstd compression or
//! decompression through `async-compression` wrappers.
//!
//! # Job specification
//!
//! The backend is selected with `"backend": "file_ops"` in the
//! [`ExecutionSpec`](aithericon_executor_domain::ExecutionSpec). The `config`
//! field is a JSON object whose `"operation"` key selects the variant:
//!
//! ```json
//! {
//!   "operation": "copy",
//!   "source": "raw/data.csv",
//!   "destination": "archive/data.csv.gz",
//!   "source_storage": {
//!     "backend": "s3",
//!     "endpoint": "https://s3.amazonaws.com",
//!     "bucket": "data-lake",
//!     "region": "us-east-1"
//!   },
//!   "compress": "gzip"
//! }
//! ```
//!
//! See [`config`] for the full schema of each operation.

pub mod backend;
pub mod config;
pub mod ops;
pub mod resolve;

pub use backend::FileOpsBackend;
pub use config::FileOpsConfig;
