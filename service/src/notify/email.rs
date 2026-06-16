//! Outbound-email seam.
//!
//! The actual subsystem — the [`Mailer`](aithericon_email::Mailer) port, the
//! typed messages, and the SMTP / Brevo / log adapters — lives in the
//! `aithericon-email` crate. This module is the thin bridge that turns the
//! service's [`AppConfig`] into a constructed mailer.
//!
//! Re-exports the crate's public surface so call sites can `use
//! crate::notify::email::{...}` without depending on the crate name directly.

use std::sync::Arc;

pub use aithericon_email::messages::{MemberAdded, ResourceShared, Welcome, WorkspaceInvite};
pub use aithericon_email::{EmailError, Mailer, Recipient};

use crate::config::AppConfig;

/// An offline log mailer with default branding. Used as the safe default and by
/// tests that build an [`crate::AppState`] without a configured provider.
pub fn log_mailer() -> Arc<dyn Mailer> {
    aithericon_email::build_mailer(&aithericon_email::MailerConfig::default())
        .expect("default log mailer is infallible")
}

/// Build the active mailer from config. Delegates provider selection (and the
/// graceful fall-back to the log mailer when creds are missing) to
/// [`aithericon_email::build_mailer`]. Returns the log mailer on a hard config
/// error so the service still boots — delivery is best-effort, never a gate.
pub fn build_mailer(config: &AppConfig) -> Arc<dyn Mailer> {
    let mailer_config = config.email.to_mailer_config();
    match aithericon_email::build_mailer(&mailer_config) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("email subsystem init failed ({e}); using log mailer");
            // Default config is always Log mode → infallible.
            aithericon_email::build_mailer(&aithericon_email::MailerConfig::default())
                .expect("default log mailer is infallible")
        }
    }
}
