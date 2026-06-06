mod chain_store;
mod datastream_credential;
mod env_store;
mod error;
mod in_memory_store;
mod resolver;
mod store;
#[cfg(feature = "vault")]
mod vault_store;

pub use chain_store::ChainedSecretStore;
#[cfg(feature = "vault")]
pub use datastream_credential::{unwrap_subject_grant, wrap_subject_grant, NATS_SUBJECT_GRANT_KEY};
pub use env_store::EnvVarSecretStore;
pub use error::SecretError;
pub use in_memory_store::InMemorySecretStore;
pub use resolver::{extract_secret_keys, resolve_secrets};
pub use store::SecretStore;
#[cfg(feature = "vault")]
pub use vault_store::{vault_unwrap_secrets, SecretWrapper, VaultSecretStore};
