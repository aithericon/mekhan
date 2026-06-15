//! SMTP adapter conformance against a real capture server (MailHog/Mailpit).
//!
//! Covers what the in-process tests can't: the lettre transport actually
//! connecting, the plaintext (`SmtpTls::None`) path a dev relay needs, the
//! byte-level MIME the server accepts, and the HTTP API confirming the message
//! landed with the rendered subject + recipient.
//!
//! Requires MailHog at `localhost:1025` (SMTP) + `localhost:8025` (HTTP API).
//! Bring it up with `just dev mailhog-up`. Ports follow the per-worktree slot
//! via `MEKHAN_MAILPIT_SMTP_PORT` / `MEKHAN_MAILPIT_UI_PORT`.
//!
//! Gated on `SMTP_E2E=1` so the default `cargo test` (and CI lanes without
//! docker) skip cleanly — same gate as the executor's `conformance_smtp`.

use std::sync::Arc;
use std::time::Duration;

use aithericon_email::messages::WorkspaceInvite;
use aithericon_email::{Branding, Mailer, Recipient, Renderer, SmtpMailer, SmtpSettings, SmtpTls};

fn smtp_port() -> u16 {
    std::env::var("MEKHAN_MAILPIT_SMTP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1025)
}

fn api_base() -> String {
    let port = std::env::var("MEKHAN_MAILPIT_UI_PORT").unwrap_or_else(|_| "8025".to_string());
    format!("http://localhost:{port}/api/v1")
}

fn enabled() -> bool {
    std::env::var("SMTP_E2E").map(|v| v == "1").unwrap_or(false)
}

/// Reachability probe so we surface a clean skip rather than a TCP timeout deep
/// in lettre when the caller forgot `just dev mailhog-up`.
async fn reachable() -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            reqwest::Client::new()
                .get(format!("{}/messages", api_base()))
                .send(),
        )
        .await,
        Ok(Ok(resp)) if resp.status().is_success()
    )
}

async fn clear() {
    let _ = reqwest::Client::new()
        .delete(format!("{}/messages", api_base()))
        .send()
        .await;
}

// MailHog `GET /api/v1/messages` shape (a JSON array).
#[derive(serde::Deserialize)]
struct MhMessage {
    #[serde(rename = "Content")]
    content: MhContent,
    #[serde(rename = "To")]
    to: Vec<MhAddr>,
}
#[derive(serde::Deserialize)]
struct MhContent {
    #[serde(rename = "Headers")]
    headers: serde_json::Value,
    #[serde(rename = "Body")]
    body: String,
}
#[derive(serde::Deserialize)]
struct MhAddr {
    #[serde(rename = "Mailbox")]
    mailbox: String,
    #[serde(rename = "Domain")]
    domain: String,
}
impl MhAddr {
    fn email(&self) -> String {
        format!("{}@{}", self.mailbox, self.domain)
    }
}

/// Poll for at least one message (the server stores asynchronously).
async fn await_messages() -> Vec<MhMessage> {
    for _ in 0..20 {
        let msgs = reqwest::Client::new()
            .get(format!("{}/messages", api_base()))
            .send()
            .await
            .expect("GET /messages")
            .json::<Vec<MhMessage>>()
            .await
            .expect("messages json");
        if !msgs.is_empty() {
            return msgs;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no message captured by MailHog within timeout");
}

#[tokio::test]
async fn smtp_invite_lands_in_mailhog() {
    if !enabled() {
        eprintln!("skipping: set SMTP_E2E=1 (and `just dev mailhog-up`) to run");
        return;
    }
    if !reachable().await {
        eprintln!(
            "skipping: MailHog API unreachable at {} — run `just dev mailhog-up`",
            api_base()
        );
        return;
    }

    clear().await;

    let renderer = Arc::new(Renderer::new(Branding::default()).unwrap());
    let mailer = SmtpMailer::new(
        renderer,
        &SmtpSettings {
            host: "localhost".into(),
            port: smtp_port(),
            username: None,
            password: None,
            tls: SmtpTls::None, // MailHog speaks plaintext on 1025
        },
        "no-reply@aithericon.test",
        "Aithericon",
    )
    .expect("smtp mailer builds");

    let msg = WorkspaceInvite {
        recipient_name: Some("Ada".into()),
        inviter_name: "Grace".into(),
        workspace_name: "Acme Labs".into(),
        role: "editor".into(),
        accept_url: "https://app.aithericon.test/invite/accept?token=tok123".into(),
        expires: "on 2026-06-22".into(),
        existing_user: false,
    };

    mailer
        .send(&Recipient::named("ada@example.com", "Ada"), &msg)
        .await
        .expect("send via mailhog");

    let msgs = await_messages().await;
    assert_eq!(msgs.len(), 1, "exactly one message");
    let m = &msgs[0];
    assert_eq!(m.to[0].email(), "ada@example.com");

    let subject = m.content.headers["Subject"][0].as_str().unwrap_or_default();
    assert_eq!(subject, "You've been invited to Acme Labs");

    // ASCII workspace name survives quoted-printable encoding in the body.
    assert!(
        m.content.body.contains("Acme"),
        "rendered body should mention the workspace"
    );

    clear().await;
}
