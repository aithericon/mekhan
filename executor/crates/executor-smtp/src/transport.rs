//! Build a `lettre` async SMTP transport from the resolved resource.
//!
//! TLS mode is picked by port convention rather than an explicit flag:
//!
//! - **587** — STARTTLS (Submission); start plaintext, upgrade after EHLO
//! - **465** — implicit TLS (legacy "smtps"); TLS from the first byte
//! - **25**  — no TLS (only sensible inside a trusted network — most public
//!             relays refuse plaintext auth on 25 now)
//!
//! Anything else fails [`SmtpOutcome::InvalidConfig`]. This matches the
//! convention documented on the `Smtp` resource type at
//! `shared/resources/src/types.rs::Smtp`.

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

/// Build the transport. Returns `Invalid(...)` when the port doesn't match
/// a supported mode or TLS params can't be constructed.
pub fn build(resource: &ResolvedSmtpResource) -> BuildResult {
    let tls = match tls_params(&resource.host) {
        Ok(t) => t,
        Err(e) => return BuildResult::Invalid(e),
    };

    let builder = match resource.port {
        587 => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&resource.host)
            .port(587)
            .tls(Tls::Required(tls)),
        465 => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&resource.host)
            .port(465)
            .tls(Tls::Wrapper(tls)),
        25 => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&resource.host)
            .port(25)
            .tls(Tls::None),
        other => {
            return BuildResult::Invalid(SmtpOutcome::InvalidConfig {
                message: format!(
                    "unsupported smtp port {other}: only 587 (STARTTLS), 465 (implicit TLS), and 25 (plain) are recognized"
                ),
            });
        }
    };

    let creds = Credentials::new(resource.username.clone(), resource.password.clone());
    let transport = builder.credentials(creds).build();
    BuildResult::Ready(transport)
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
    fn unknown_port_rejected_with_invalid_config() {
        match build(&r(2525)) {
            BuildResult::Invalid(SmtpOutcome::InvalidConfig { message }) => {
                assert!(message.contains("2525"));
                assert!(message.contains("587"));
                assert!(message.contains("465"));
                assert!(message.contains("25"));
            }
            _ => panic!("expected InvalidConfig for unknown port"),
        }
    }
}
