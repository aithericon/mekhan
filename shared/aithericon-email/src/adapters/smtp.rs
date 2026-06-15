//! SMTP adapter backed by `lettre` (async, rustls).
//!
//! TLS follows [`SmtpTls`]: `Auto` infers from the port (465 ⇒ implicit TLS,
//! else STARTTLS), or it can be pinned to `Implicit` / `StartTls` / `None`
//! (plaintext for dev relays like MailHog). Credentials are optional (some
//! internal relays accept unauthenticated submission).

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Message, Tokio1Executor};

use crate::config::{SmtpSettings, SmtpTls};
use crate::error::{EmailError, Result};
use crate::port::{Mailer, Recipient, TemplateMessage};
use crate::render::Renderer;

pub struct SmtpMailer {
    renderer: Arc<Renderer>,
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpMailer {
    /// Build the transport from settings. Fails on an unparseable from-address
    /// or a relay-builder error (surfaced at startup, not per-send).
    pub fn new(
        renderer: Arc<Renderer>,
        settings: &SmtpSettings,
        from_address: &str,
        from_name: &str,
    ) -> Result<Self> {
        let from: Mailbox = format!("{from_name} <{from_address}>")
            .parse()
            .map_err(|e| EmailError::InvalidAddress(format!("from address: {e}")))?;

        // Resolve `Auto` from the port: 465 ⇒ implicit TLS, else STARTTLS.
        let tls = match settings.tls {
            SmtpTls::Auto if settings.port == 465 => SmtpTls::Implicit,
            SmtpTls::Auto => SmtpTls::StartTls,
            explicit => explicit,
        };

        let mut builder = match tls {
            SmtpTls::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&settings.host)
                .map_err(|e| EmailError::Config(format!("smtp relay: {e}")))?,
            SmtpTls::StartTls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&settings.host)
                    .map_err(|e| EmailError::Config(format!("smtp starttls relay: {e}")))?
            }
            // Plaintext (dev relays). `builder_dangerous` = no TLS.
            SmtpTls::None | SmtpTls::Auto => {
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&settings.host)
            }
        }
        .port(settings.port);

        if let (Some(user), Some(pass)) = (&settings.username, &settings.password) {
            builder = builder.credentials(Credentials::new(user.clone(), pass.clone()));
        }

        Ok(Self {
            renderer,
            transport: builder.build(),
            from,
        })
    }
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn send(&self, to: &Recipient, message: &dyn TemplateMessage) -> Result<()> {
        let rendered = self.renderer.render(message)?;

        let to_mbox: Mailbox = match &to.name {
            Some(name) => format!("{name} <{}>", to.email),
            None => to.email.clone(),
        }
        .parse()
        .map_err(|e| EmailError::InvalidAddress(format!("recipient {}: {e}", to.email)))?;

        let email = Message::builder()
            .from(self.from.clone())
            .to(to_mbox)
            .subject(rendered.subject)
            .multipart(MultiPart::alternative_plain_html(
                html_to_text(&rendered.html),
                rendered.html,
            ))
            .map_err(|e| EmailError::Transport(format!("building message: {e}")))?;

        self.transport
            .send(email)
            .await
            .map_err(|e| EmailError::Transport(format!("smtp send: {e}")))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Crude HTML→text fallback for the `text/plain` alternative part. Good enough
/// for a readable fallback; clients overwhelmingly render the HTML part.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Branding;
    use std::sync::Arc;

    fn renderer() -> Arc<Renderer> {
        Arc::new(Renderer::new(Branding::default()).unwrap())
    }

    fn settings(port: u16, tls: SmtpTls) -> SmtpSettings {
        SmtpSettings {
            host: "localhost".into(),
            port,
            username: None,
            password: None,
            tls,
        }
    }

    #[test]
    fn builds_transport_for_every_tls_mode() {
        // Construction wires the transport but never connects — all modes,
        // including Auto's port-based inference, must build cleanly.
        for (port, tls) in [
            (465, SmtpTls::Auto),
            (587, SmtpTls::Auto),
            (465, SmtpTls::Implicit),
            (587, SmtpTls::StartTls),
            (1025, SmtpTls::None),
        ] {
            assert!(
                SmtpMailer::new(renderer(), &settings(port, tls), "from@x.test", "From").is_ok(),
                "tls={tls:?} port={port} should build"
            );
        }
    }

    #[test]
    fn rejects_unparseable_from_address() {
        let err = SmtpMailer::new(
            renderer(),
            &settings(587, SmtpTls::Auto),
            "not an address",
            "X",
        );
        assert!(matches!(err, Err(EmailError::InvalidAddress(_))));
    }

    #[test]
    fn html_to_text_strips_tags_and_collapses_whitespace() {
        let text = html_to_text("<h1>Hi</h1>\n  <p>line  one</p>");
        assert_eq!(text, "Hi line one");
    }
}
