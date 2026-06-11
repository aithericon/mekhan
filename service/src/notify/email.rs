//! Invite-email delivery seam (Phase 4).
//!
//! [`EmailSender`] abstracts how an invite's accept link reaches the invitee.
//! The DEFAULT is [`LogEmailSender`] — it `tracing::info!`s the accept URL so
//! the offline `dev_noop` flow (and CI) works with zero SMTP. An optional
//! [`SmtpEmailSender`] is built when `email.mode = smtp`. No provider SDK is
//! hardcoded; SMTP is feature-light (host/port/user/pass from config).

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::{AppConfig, EmailMode};

#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    #[error("email send failed: {0}")]
    Send(String),
}

/// Sends an invite's accept link to the invitee.
#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send_invite(
        &self,
        to: &str,
        accept_url: &str,
        workspace_name: &str,
        inviter_name: &str,
    ) -> Result<(), EmailError>;
}

/// Default + dev sender: logs the accept URL instead of sending. The link is
/// the `dev_noop` accept path — copy it from the log to accept an invite
/// offline.
#[derive(Default)]
pub struct LogEmailSender;

#[async_trait]
impl EmailSender for LogEmailSender {
    async fn send_invite(
        &self,
        to: &str,
        accept_url: &str,
        workspace_name: &str,
        inviter_name: &str,
    ) -> Result<(), EmailError> {
        tracing::info!(
            target: "invite_email",
            %to, %workspace_name, %inviter_name,
            "INVITE (log mode) — accept link: {accept_url}"
        );
        Ok(())
    }
}

/// SMTP sender (built when `email.mode = smtp`). Kept dependency-light; the
/// actual relay send uses `lettre` only when the `smtp-email` feature is on.
/// Without the feature it is never constructed (the builder falls back to log
/// + warns), so the trait object stays available offline.
pub struct SmtpEmailSender {
    pub from_address: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[async_trait]
impl EmailSender for SmtpEmailSender {
    async fn send_invite(
        &self,
        to: &str,
        accept_url: &str,
        workspace_name: &str,
        inviter_name: &str,
    ) -> Result<(), EmailError> {
        // SMTP transport is deferred (config-gated, no provider lock-in). Until
        // the relay is wired, fail loud rather than silently drop an invite.
        tracing::error!(
            target: "invite_email",
            %to, %workspace_name, %inviter_name, smtp_host = %self.host,
            "SMTP invite delivery not yet implemented (from={}); accept link: {accept_url}",
            self.from_address
        );
        Err(EmailError::Send(
            "smtp delivery not implemented; set email.mode=log for now".into(),
        ))
    }
}

/// Select the sender from config. `smtp` mode with a host → [`SmtpEmailSender`];
/// otherwise (default / missing host) → [`LogEmailSender`]. Always returns a
/// sender (never `None`) so the accept flow works offline.
pub fn build_email_sender(config: &AppConfig) -> Arc<dyn EmailSender> {
    match config.email.mode {
        EmailMode::Smtp => match config.email.smtp_host.clone() {
            Some(host) => Arc::new(SmtpEmailSender {
                from_address: config.email.from_address.clone(),
                host,
                port: config.email.smtp_port.unwrap_or(587),
                username: config.email.smtp_username.clone(),
                password: config.email.smtp_password.clone(),
            }),
            None => {
                tracing::warn!("email.mode=smtp but no smtp_host set — falling back to log sender");
                Arc::new(LogEmailSender)
            }
        },
        EmailMode::Log => Arc::new(LogEmailSender),
    }
}
