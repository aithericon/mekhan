//! The single error type the [`Mailer`](crate::Mailer) port surfaces.
//!
//! Adapters flatten provider-specific failures (lettre transport errors, Brevo
//! HTTP status codes, …) into these variants so callers depend only on the
//! port, never on a concrete provider's error type.

/// Anything that can go wrong rendering or delivering an email.
#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    /// The transport (SMTP relay, Brevo API, …) rejected or failed the send.
    #[error("email transport error: {0}")]
    Transport(String),

    /// A Tera template failed to render (missing key, syntax error, …).
    #[error("email template render error: {0}")]
    Render(String),

    /// A recipient / sender address was malformed.
    #[error("invalid email address: {0}")]
    InvalidAddress(String),

    /// The adapter was constructed with an incomplete/invalid configuration.
    #[error("email configuration error: {0}")]
    Config(String),
}

/// Convenience alias for fallible email operations.
pub type Result<T> = std::result::Result<T, EmailError>;
