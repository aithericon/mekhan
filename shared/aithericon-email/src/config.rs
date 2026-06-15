//! Runtime configuration for the email subsystem.
//!
//! The host application (mekhan-service) owns its own config format; it maps
//! that onto [`MailerConfig`] and hands it to [`crate::build_mailer`], which
//! picks the adapter. Keeping this type here means the crate is self-describing
//! and unit-testable without the service.

/// Which transport to use. Selected at runtime — all adapters are compiled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmailProvider {
    /// Log the rendered email to `tracing` instead of sending. Offline default.
    #[default]
    Log,
    /// Deliver via an SMTP relay (lettre).
    Smtp,
    /// Deliver via the Brevo transactional HTTP API.
    Brevo,
}

impl EmailProvider {
    /// Lenient parse for config strings / env vars.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "smtp" | "lettre" => Self::Smtp,
            "brevo" | "sendinblue" => Self::Brevo,
            _ => Self::Log,
        }
    }
}

/// How the SMTP connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SmtpTls {
    /// Infer from the port: 465 ⇒ implicit TLS, anything else ⇒ STARTTLS.
    #[default]
    Auto,
    /// Implicit TLS from connect (SMTPS, typically port 465).
    Implicit,
    /// Upgrade a plaintext connection via STARTTLS (typically port 587).
    StartTls,
    /// No encryption — for local dev relays (MailHog/Mailpit on 1025) or an
    /// internal plaintext relay. Never use against a public relay.
    None,
}

impl SmtpTls {
    /// Lenient parse for config strings.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "implicit" | "tls" | "smtps" => Self::Implicit,
            "starttls" => Self::StartTls,
            "none" | "plain" | "plaintext" | "insecure" => Self::None,
            _ => Self::Auto,
        }
    }
}

/// SMTP relay settings (read only when `provider == Smtp`).
#[derive(Debug, Clone)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub tls: SmtpTls,
}

/// Brevo settings (read only when `provider == Brevo`).
#[derive(Debug, Clone)]
pub struct BrevoSettings {
    pub api_key: String,
}

/// Per-deployment branding merged into every template's context, so individual
/// messages don't repeat product/footer boilerplate.
#[derive(Debug, Clone)]
pub struct Branding {
    /// Product name shown in headers/footers/subjects, e.g. "Aithericon".
    pub product_name: String,
    /// Public origin links are built against, e.g. `https://app.aithericon.com`.
    pub base_url: String,
    /// Support / reply-to address surfaced in the footer.
    pub support_address: String,
}

impl Default for Branding {
    fn default() -> Self {
        Self {
            product_name: "Aithericon".to_string(),
            base_url: "http://localhost:15173".to_string(),
            support_address: "support@aithericon.com".to_string(),
        }
    }
}

/// Everything [`crate::build_mailer`] needs to construct the active adapter.
#[derive(Debug, Clone)]
pub struct MailerConfig {
    pub provider: EmailProvider,
    /// Envelope-from address on outgoing mail.
    pub from_address: String,
    /// Envelope-from display name.
    pub from_name: String,
    pub branding: Branding,
    pub smtp: Option<SmtpSettings>,
    pub brevo: Option<BrevoSettings>,
}

impl Default for MailerConfig {
    fn default() -> Self {
        let branding = Branding::default();
        Self {
            provider: EmailProvider::Log,
            from_address: "no-reply@aithericon.local".to_string(),
            from_name: branding.product_name.clone(),
            branding,
            smtp: None,
            brevo: None,
        }
    }
}
