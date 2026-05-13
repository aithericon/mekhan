/// Errors that can occur during secret resolution.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SecretError {
    #[error("secret not found: {0}")]
    NotFound(String),

    #[error("secret store unavailable: {0}")]
    StoreUnavailable(String),

    #[error("access denied for secret: {0}")]
    AccessDenied(String),
}
