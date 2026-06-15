//! The hexagonal port: [`Mailer`] (driven adapter boundary) and
//! [`TemplateMessage`] (the typed payload that maps to a Tera template).
//!
//! The domain depends only on these traits. SMTP, Brevo and the dev log sender
//! are interchangeable adapters behind `Arc<dyn Mailer>`.

use async_trait::async_trait;

use crate::error::Result;

/// An email recipient — address plus an optional display name.
#[derive(Debug, Clone)]
pub struct Recipient {
    pub email: String,
    pub name: Option<String>,
}

impl Recipient {
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            name: None,
        }
    }

    pub fn named(email: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            name: Some(name.into()),
        }
    }

    /// Display name if present, else the raw address — handy for greetings.
    pub fn display(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.email)
    }
}

/// A strongly-typed email payload. Each concrete message (invite, share, …)
/// names its Tera template, its subject, and the variables that template needs.
///
/// Implementors live in [`crate::messages`]. The shared branding context
/// (product name, base URL, year, …) is merged in by the renderer, so message
/// structs only carry their own fields.
pub trait TemplateMessage: Send + Sync {
    /// Template file stem under `templates/` (without the `.html` suffix),
    /// e.g. `"workspace_invite"`.
    fn template(&self) -> &'static str;

    /// The rendered subject line.
    fn subject(&self) -> String;

    /// Template variables for this message.
    fn context(&self) -> tera::Context;

    /// BCP-47 locale hint. English-only today; carried so Fluent/i18n can be
    /// layered in later without changing the port.
    fn locale(&self) -> &str {
        "en"
    }
}

/// The driven port every adapter implements. Construct one via
/// [`crate::build_mailer`] and inject `Arc<dyn Mailer>` into the app.
#[async_trait]
pub trait Mailer: Send + Sync {
    /// Render `message` and deliver it to `to`.
    async fn send(&self, to: &Recipient, message: &dyn TemplateMessage) -> Result<()>;

    /// Escape hatch for tests to downcast to a concrete adapter (e.g. the
    /// capturing mailer). Adapters return `self`.
    fn as_any(&self) -> &dyn std::any::Any;
}
