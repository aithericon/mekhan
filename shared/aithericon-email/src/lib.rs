//! # aithericon-email
//!
//! A hexagonal transactional-email subsystem.
//!
//! - **Port:** [`Mailer`] — `async fn send(to, message)`. Inject as
//!   `Arc<dyn Mailer>`; callers never see the provider.
//! - **Messages:** strongly-typed [`messages`] structs (invite, share, member,
//!   welcome) implementing [`TemplateMessage`]; rendered by [`Renderer`] from
//!   Tera templates embedded at compile time.
//! - **Adapters:** [`SmtpMailer`] (lettre), [`BrevoMailer`] (HTTP), and the
//!   dev/test [`LogMailer`] / [`CapturingMailer`].
//! - **Wiring:** [`build_mailer`] selects the adapter from [`MailerConfig`].
//!
//! ```no_run
//! use std::sync::Arc;
//! use aithericon_email::{build_mailer, MailerConfig, Recipient};
//! use aithericon_email::messages::Welcome;
//!
//! # async fn demo() -> aithericon_email::Result<()> {
//! let mailer = build_mailer(&MailerConfig::default())?;
//! let msg = Welcome {
//!     user_name: "Ada".into(),
//!     workspace_name: Some("Acme Labs".into()),
//!     login_url: "https://app.aithericon.com".into(),
//! };
//! mailer.send(&Recipient::new("ada@example.com"), &msg).await?;
//! # Ok(()) }
//! ```

pub mod adapters;
pub mod config;
pub mod error;
pub mod messages;
pub mod port;
pub mod render;

use std::sync::Arc;

pub use adapters::{BrevoMailer, CapturedEmail, CapturingMailer, LogMailer, SmtpMailer};
pub use config::{Branding, BrevoSettings, EmailProvider, MailerConfig, SmtpSettings, SmtpTls};
pub use error::{EmailError, Result};
pub use port::{Mailer, Recipient, TemplateMessage};
pub use render::{RenderedEmail, Renderer};

/// Construct the active mailer from config.
///
/// The renderer (with branding) is built once and shared into the adapter.
/// If a provider is selected but its credentials are missing/invalid, this logs
/// a warning and falls back to [`LogMailer`] so the app still boots and the
/// offline path keeps working — delivery is best-effort, never a startup gate.
pub fn build_mailer(config: &MailerConfig) -> Result<Arc<dyn Mailer>> {
    let renderer = Arc::new(Renderer::new(config.branding.clone())?);

    let mailer: Arc<dyn Mailer> = match config.provider {
        EmailProvider::Log => Arc::new(LogMailer::new(renderer)),

        EmailProvider::Smtp => match &config.smtp {
            Some(smtp) => match SmtpMailer::new(
                renderer.clone(),
                smtp,
                &config.from_address,
                &config.from_name,
            ) {
                Ok(m) => Arc::new(m),
                Err(e) => {
                    tracing::warn!("smtp mailer init failed ({e}); falling back to log mailer");
                    Arc::new(LogMailer::new(renderer))
                }
            },
            None => {
                tracing::warn!("provider=smtp but no smtp settings; falling back to log mailer");
                Arc::new(LogMailer::new(renderer))
            }
        },

        EmailProvider::Brevo => match &config.brevo {
            Some(brevo) => match BrevoMailer::new(
                renderer.clone(),
                brevo,
                &config.from_address,
                &config.from_name,
            ) {
                Ok(m) => Arc::new(m),
                Err(e) => {
                    tracing::warn!("brevo mailer init failed ({e}); falling back to log mailer");
                    Arc::new(LogMailer::new(renderer))
                }
            },
            None => {
                tracing::warn!("provider=brevo but no brevo settings; falling back to log mailer");
                Arc::new(LogMailer::new(renderer))
            }
        },
    };
    Ok(mailer)
}
