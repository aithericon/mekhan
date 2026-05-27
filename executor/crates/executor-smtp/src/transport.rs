//! Build a `lettre` async SMTP transport from the resolved resource.
//!
//! TLS mode is picked by port convention:
//!
//! - **465** — implicit TLS (legacy "smtps"); TLS from the first byte
//! - **587** — STARTTLS required (Submission); refuse to deliver without it
//! - **25, 1025, 2525** — plain (no TLS) by default; covers public MX (25),
//!   mailhog/maildev/mailpit (1025), and the common "alt submission"
//!   convention (2525). The dev catcher ports are explicitly listed because
//!   `just dev mailhog-up` is the documented SMTP local loop.
//! - **anything else** — Opportunistic STARTTLS (upgrade if EHLO advertises
//!   it, fall back to plain otherwise). This is the right default for the
//!   long tail of self-hosted relays without forcing every workflow author
//!   to learn a TLS-mode taxonomy.
//!
//! Credentials are only attached when BOTH `username` and `password` are
//! non-empty. Local dev catchers (mailhog) accept anonymous SMTP; passing
//! empty `AUTH PLAIN` makes some real servers reject the dialog.

use aithericon_executor_backend_configs::smtp::ResolvedSmtpResource;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, Tokio1Executor};

use crate::outcome::SmtpOutcome;

/// Result of [`build`] — either the lettre transport ready to send, or the
/// outcome to attach to the run when the resource config is unusable.
pub enum BuildResult {
    Ready(AsyncSmtpTransport<Tokio1Executor>),
    Invalid(SmtpOutcome),
}

/// Build the transport. Returns `Invalid(...)` only when TLS parameters
/// can't be constructed for the host — port choice is permissive.
pub fn build(resource: &ResolvedSmtpResource) -> BuildResult {
    let tls = match tls_params(&resource.host) {
        Ok(t) => t,
        Err(e) => return BuildResult::Invalid(e),
    };

    let builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&resource.host)
        .port(resource.port)
        .tls(match resource.port {
            465 => Tls::Wrapper(tls),
            587 => Tls::Required(tls),
            25 | 1025 | 2525 => Tls::None,
            _ => Tls::Opportunistic(tls),
        });

    let builder = if !resource.username.is_empty() && !resource.password.is_empty() {
        let creds = Credentials::new(resource.username.clone(), resource.password.clone());
        builder.credentials(creds)
    } else {
        builder
    };
    BuildResult::Ready(builder.build())
}

fn tls_params(host: &str) -> Result<TlsParameters, SmtpOutcome> {
    TlsParameters::new(host.to_string()).map_err(|e| SmtpOutcome::TlsFailed {
        error: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(port: u16) -> ResolvedSmtpResource {
        ResolvedSmtpResource {
            host: "smtp.example.com".into(),
            port,
            username: "u".into(),
            password: "p".into(),
            from_address: None,
        }
    }

    #[test]
    fn known_ports_succeed() {
        assert!(matches!(build(&r(587)), BuildResult::Ready(_)));
        assert!(matches!(build(&r(465)), BuildResult::Ready(_)));
        assert!(matches!(build(&r(25)), BuildResult::Ready(_)));
    }

    #[test]
    fn dev_catcher_ports_succeed() {
        // mailhog / maildev / mailpit + the alt-submission convention need
        // to be first-class so `just dev mailhog-up` is a complete loop.
        assert!(matches!(build(&r(1025)), BuildResult::Ready(_)));
        assert!(matches!(build(&r(2525)), BuildResult::Ready(_)));
    }

    #[test]
    fn unknown_ports_fall_back_to_opportunistic() {
        // Long-tail self-hosted relays shouldn't fail at config time —
        // STARTTLS opportunistic upgrades when offered, plain otherwise.
        assert!(matches!(build(&r(8025)), BuildResult::Ready(_)));
    }

    #[test]
    fn blank_credentials_are_omitted() {
        // Mailhog accepts anonymous SMTP; passing AUTH PLAIN with empty
        // strings makes some real servers reject the dialog. Verified by
        // construction here — the builder branch covered, the dialog test
        // lives in tests.rs via the MessageSink seam.
        let mut res = r(1025);
        res.username = String::new();
        res.password = String::new();
        assert!(matches!(build(&res), BuildResult::Ready(_)));
    }
}
