//! Render + adapter-selection tests. These exercise the real Tera templates
//! (so a broken template fails the build) and the capturing/log mailers.

use std::sync::Arc;

use aithericon_email::messages::{MemberAdded, ResourceShared, Welcome, WorkspaceInvite};
use aithericon_email::{
    build_mailer, Branding, CapturingMailer, EmailProvider, Mailer, MailerConfig, Recipient,
    Renderer,
};

fn renderer() -> Arc<Renderer> {
    Arc::new(Renderer::new(Branding::default()).expect("renderer builds"))
}

#[test]
fn workspace_invite_renders_with_fields_and_branding() {
    let r = renderer();
    let out = r
        .render(&WorkspaceInvite {
            recipient_name: Some("Ada".into()),
            inviter_name: "Grace".into(),
            workspace_name: "Acme Labs".into(),
            role: "editor".into(),
            accept_url: "https://app.aithericon.com/invite/accept?token=abc".into(),
            expires: "in 7 days".into(),
            existing_user: false,
        })
        .expect("renders");

    assert_eq!(out.subject, "You've been invited to Acme Labs");
    assert!(out.html.contains("Grace"));
    assert!(out.html.contains("Acme Labs"));
    assert!(out.html.contains("editor"));
    assert!(out
        .html
        .contains("https://app.aithericon.com/invite/accept?token=abc"));
    // Branding merged from the renderer, not the message.
    assert!(out.html.contains("Aithericon"));
}

#[test]
fn untrusted_fields_are_escaped_but_urls_are_not() {
    // A name carrying HTML must be escaped (no injection), while the trusted
    // accept URL keeps its slashes (rendered via `| safe`).
    let out = renderer()
        .render(&WorkspaceInvite {
            recipient_name: None,
            inviter_name: "<script>alert(1)</script>".into(),
            workspace_name: "Acme".into(),
            role: "viewer".into(),
            accept_url: "https://app.aithericon.com/invite/accept?token=t".into(),
            expires: "soon".into(),
            existing_user: true,
        })
        .expect("renders");
    assert!(!out.html.contains("<script>alert(1)</script>"));
    assert!(out.html.contains("&lt;script&gt;"));
    assert!(out
        .html
        .contains("https://app.aithericon.com/invite/accept?token=t"));
}

#[test]
fn all_message_kinds_render() {
    let r = renderer();
    r.render(&ResourceShared {
        recipient_name: None,
        sharer_name: "Grace".into(),
        object_kind: "template".into(),
        object_name: "Flow A".into(),
        role: "viewer".into(),
        workspace_name: "Acme".into(),
        url: "https://x/y".into(),
    })
    .expect("resource_shared");

    r.render(&MemberAdded {
        recipient_name: Some("Ada".into()),
        actor_name: "Grace".into(),
        workspace_name: "Acme".into(),
        role: "admin".into(),
        url: "https://x".into(),
        role_changed: true,
    })
    .expect("member_added");

    r.render(&Welcome {
        user_name: "Ada".into(),
        workspace_name: Some("Acme".into()),
        login_url: "https://x".into(),
    })
    .expect("welcome");
}

#[tokio::test]
async fn capturing_mailer_records_sends() {
    let mailer = CapturingMailer::new(renderer());
    mailer
        .send(
            &Recipient::named("ada@example.com", "Ada"),
            &Welcome {
                user_name: "Ada".into(),
                workspace_name: None,
                login_url: "https://x".into(),
            },
        )
        .await
        .expect("send");

    let last = mailer.last().expect("one email");
    assert_eq!(last.to, "ada@example.com");
    assert_eq!(last.template, "welcome");
    assert_eq!(last.subject, "Welcome to Aithericon");
}

#[test]
fn build_mailer_defaults_to_log_and_falls_back_when_creds_missing() {
    // Default = log.
    let log = build_mailer(&MailerConfig::default()).expect("log mailer");
    assert!(log
        .as_any()
        .downcast_ref::<aithericon_email::LogMailer>()
        .is_some());

    // Provider=Brevo but no settings ⇒ graceful fallback to log.
    let cfg = MailerConfig {
        provider: EmailProvider::Brevo,
        ..Default::default()
    };
    let fell_back = build_mailer(&cfg).expect("falls back");
    assert!(fell_back
        .as_any()
        .downcast_ref::<aithericon_email::LogMailer>()
        .is_some());
}
