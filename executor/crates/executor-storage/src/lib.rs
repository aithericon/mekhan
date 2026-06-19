pub mod brokered;
pub mod config;
pub mod local;
pub mod traits;

#[cfg(feature = "opendal")]
pub mod opendal;

pub use brokered::BrokeredArtifactStore;
pub use config::{StorageBackend, StorageConfig, StorageCredentials};
pub use local::LocalArtifactStore;
pub use traits::{ArtifactStore, StorageError, StoragePath, UploadOptions};

#[cfg(feature = "opendal")]
pub use self::opendal::{build_operator, build_operator_with_prefix, OpenDalArtifactStore};
