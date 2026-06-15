//! Dev / offline adapters that never touch the network.
//!
//! - [`LogMailer`] renders the email and emits it to `tracing` — the default so
//!   the offline `dev_noop` flow and CI work with zero SMTP/Brevo creds.
//! - [`CapturingMailer`] keeps rendered emails in memory for assertions in
//!   tests (reach it via [`Mailer::as_any`]).

use std::any::Any;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::error::Result;
use crate::port::{Mailer, Recipient, TemplateMessage};
use crate::render::Renderer;

/// Renders then logs each email instead of sending it.
pub struct LogMailer {
    renderer: Arc<Renderer>,
}

impl LogMailer {
    pub fn new(renderer: Arc<Renderer>) -> Self {
        Self { renderer }
    }
}

#[async_trait]
impl Mailer for LogMailer {
    async fn send(&self, to: &Recipient, message: &dyn TemplateMessage) -> Result<()> {
        let rendered = self.renderer.render(message)?;
        tracing::info!(
            target: "email",
            to = %to.email,
            template = message.template(),
            subject = %rendered.subject,
            "EMAIL (log mode) — not sent; rendered {} bytes of HTML",
            rendered.html.len(),
        );
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// One captured email.
#[derive(Debug, Clone)]
pub struct CapturedEmail {
    pub to: String,
    pub template: &'static str,
    pub subject: String,
    pub html: String,
}

/// In-memory mailer for tests — renders for real (so template bugs surface) and
/// records each send.
#[derive(Clone, Default)]
pub struct CapturingMailer {
    renderer: Option<Arc<Renderer>>,
    sent: Arc<Mutex<Vec<CapturedEmail>>>,
}

impl CapturingMailer {
    pub fn new(renderer: Arc<Renderer>) -> Self {
        Self {
            renderer: Some(renderer),
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// All emails sent so far, in order.
    pub fn captured(&self) -> Vec<CapturedEmail> {
        self.sent.lock().expect("capture lock").clone()
    }

    /// The most recent email, if any.
    pub fn last(&self) -> Option<CapturedEmail> {
        self.sent.lock().expect("capture lock").last().cloned()
    }
}

#[async_trait]
impl Mailer for CapturingMailer {
    async fn send(&self, to: &Recipient, message: &dyn TemplateMessage) -> Result<()> {
        let rendered = self
            .renderer
            .as_ref()
            .expect("CapturingMailer built without a renderer")
            .render(message)?;
        self.sent.lock().expect("capture lock").push(CapturedEmail {
            to: to.email.clone(),
            template: message.template(),
            subject: rendered.subject,
            html: rendered.html,
        });
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
