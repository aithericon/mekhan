//! Build the MIME tree from rendered subject/body parts + attachments.
//!
//! Decision tree:
//!
//! ```text
//! text only            → text/plain
//! html only            → text/html
//! both                 → multipart/alternative { text, html }
//! + attachments        → multipart/mixed { <body>, attachment, … }
//! ```
//!
//! Address strings must already be Tera-rendered before they get here. We
//! parse each through `lettre::message::Mailbox` so "Name <addr@x.io>" and
//! bare "addr@x.io" both work, and surface a structured
//! [`SmtpOutcome::InvalidAddress`] otherwise.

use std::path::Path;
use std::str::FromStr;

use aithericon_executor_backend_configs::smtp::AttachmentSpec;
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::Message;

use crate::outcome::SmtpOutcome;

/// Soft cap on combined attachment bytes (25 MB — RFC-5321 doesn't set one,
/// but most ISPs reject anything over ~25 MB). Hard-fail with
/// [`SmtpOutcome::AttachmentError`] above this rather than wait for the
/// server to bounce.
pub const ATTACHMENT_SIZE_CAP_BYTES: u64 = 25 * 1024 * 1024;

/// One assembled message + the actual list of `To:` addresses (used by
/// the outcome envelope so the renderer can list them).
pub struct Assembled {
    pub message: Message,
    pub to_addresses: Vec<String>,
    pub cc_addresses: Vec<String>,
    pub bcc_addresses: Vec<String>,
    pub from_address: String,
}

/// Inputs for assembly. Bodies are already-rendered template output; address
/// strings are already-rendered Tera output.
pub struct Inputs<'a> {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub attachments: &'a [LoadedAttachment],
}

/// One on-disk attachment loaded into memory + metadata. Built by
/// [`load_attachments`] right before assembly.
pub struct LoadedAttachment {
    pub filename: String,
    pub bytes: Vec<u8>,
    pub content_type: ContentType,
}

/// Read each attachment from `staged_inputs`. Caller passes the SMTP config's
/// attachment list and the run context's staged-inputs map. Failures map to
/// [`SmtpOutcome::AttachmentError`].
pub fn load_attachments(
    specs: &[AttachmentSpec],
    staged_inputs: &std::collections::HashMap<String, std::path::PathBuf>,
) -> Result<Vec<LoadedAttachment>, SmtpOutcome> {
    let mut total = 0u64;
    let mut out = Vec::with_capacity(specs.len());
    for spec in specs {
        let path = staged_inputs.get(&spec.input_name).ok_or_else(|| {
            SmtpOutcome::AttachmentError {
                filename: spec.filename.clone(),
                error: format!(
                    "compiler did not stage attachment input '{}' — this is a backend/compiler mismatch",
                    spec.input_name
                ),
            }
        })?;
        let bytes = std::fs::read(path).map_err(|e| SmtpOutcome::AttachmentError {
            filename: spec.filename.clone(),
            error: format!("read {}: {e}", path.display()),
        })?;
        total = total.saturating_add(bytes.len() as u64);
        if total > ATTACHMENT_SIZE_CAP_BYTES {
            return Err(SmtpOutcome::AttachmentError {
                filename: spec.filename.clone(),
                error: format!(
                    "combined attachment size exceeds {} bytes cap",
                    ATTACHMENT_SIZE_CAP_BYTES
                ),
            });
        }
        let content_type = pick_content_type(spec.mime.as_deref(), &spec.filename)?;
        out.push(LoadedAttachment {
            filename: spec.filename.clone(),
            bytes,
            content_type,
        });
    }
    Ok(out)
}

fn pick_content_type(explicit: Option<&str>, filename: &str) -> Result<ContentType, SmtpOutcome> {
    if let Some(mt) = explicit {
        return ContentType::parse(mt).map_err(|e| SmtpOutcome::AttachmentError {
            filename: filename.to_string(),
            error: format!("invalid mime '{mt}': {e}"),
        });
    }
    let guessed = mime_from_extension(Path::new(filename).extension().and_then(|s| s.to_str()));
    ContentType::parse(guessed).map_err(|e| SmtpOutcome::AttachmentError {
        filename: filename.to_string(),
        error: format!("internal mime parse failure for '{guessed}': {e}"),
    })
}

fn mime_from_extension(ext: Option<&str>) -> &'static str {
    match ext.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain; charset=utf-8",
        Some("csv") => "text/csv; charset=utf-8",
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("json") => "application/json",
        Some("zip") => "application/zip",
        Some("xml") => "application/xml",
        _ => "application/octet-stream",
    }
}

/// Parse + validate one address field. `which` is the outcome label.
fn parse_addr(value: &str, which: &str) -> Result<Mailbox, SmtpOutcome> {
    Mailbox::from_str(value.trim()).map_err(|e| SmtpOutcome::InvalidAddress {
        field: which.to_string(),
        value: value.to_string(),
        error: e.to_string(),
    })
}

/// Assemble the message. Returns an envelope-style struct so the caller can
/// surface the actual recipient list in the success outcome.
pub fn build(inputs: Inputs<'_>) -> Result<Assembled, SmtpOutcome> {
    let from = parse_addr(&inputs.from, "from")?;
    let mut builder = Message::builder().from(from.clone()).subject(inputs.subject.clone());

    let mut to_norm = Vec::with_capacity(inputs.to.len());
    for addr in &inputs.to {
        let mb = parse_addr(addr, "to")?;
        to_norm.push(mb.to_string());
        builder = builder.to(mb);
    }
    let mut cc_norm = Vec::with_capacity(inputs.cc.len());
    for addr in &inputs.cc {
        let mb = parse_addr(addr, "cc")?;
        cc_norm.push(mb.to_string());
        builder = builder.cc(mb);
    }
    let mut bcc_norm = Vec::with_capacity(inputs.bcc.len());
    for addr in &inputs.bcc {
        let mb = parse_addr(addr, "bcc")?;
        bcc_norm.push(mb.to_string());
        builder = builder.bcc(mb);
    }

    let message = match (
        inputs.body_text.as_deref(),
        inputs.body_html.as_deref(),
        inputs.attachments.is_empty(),
    ) {
        // text-only, no attachments
        (Some(text), None, true) => builder
            .header(ContentType::TEXT_PLAIN)
            .body(text.to_string())
            .map_err(invalid_config)?,

        // html-only, no attachments
        (None, Some(html), true) => builder
            .header(ContentType::TEXT_HTML)
            .body(html.to_string())
            .map_err(invalid_config)?,

        // text + html, no attachments
        (Some(text), Some(html), true) => builder
            .multipart(MultiPart::alternative_plain_html(
                text.to_string(),
                html.to_string(),
            ))
            .map_err(invalid_config)?,

        // any body shape, with attachments → multipart/mixed
        (text_opt, html_opt, false) => {
            // Seed the outer mixed with the body. text+html nests an inner
            // multipart/alternative; single-body cases drop a SinglePart in
            // directly. Then append each attachment as a SinglePart.
            let mut mixed = match (text_opt, html_opt) {
                (Some(text), None) => MultiPart::mixed().singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_PLAIN)
                        .body(text.to_string()),
                ),
                (None, Some(html)) => MultiPart::mixed().singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_HTML)
                        .body(html.to_string()),
                ),
                (Some(text), Some(html)) => MultiPart::mixed().multipart(
                    MultiPart::alternative_plain_html(text.to_string(), html.to_string()),
                ),
                // validated upstream by SmtpConfig::validate, but keep the
                // arm so future refactors don't introduce a silent empty body.
                (None, None) => {
                    return Err(SmtpOutcome::InvalidConfig {
                        message: "smtp build: no body provided".into(),
                    });
                }
            };
            for a in inputs.attachments {
                mixed = mixed.singlepart(
                    Attachment::new(a.filename.clone()).body(a.bytes.clone(), a.content_type.clone()),
                );
            }
            builder.multipart(mixed).map_err(invalid_config)?
        }

        // No body at all → caught by SmtpConfig::validate(), but defensive.
        (None, None, true) => {
            return Err(SmtpOutcome::InvalidConfig {
                message: "smtp build: no body provided".into(),
            });
        }
    };

    Ok(Assembled {
        message,
        to_addresses: to_norm,
        cc_addresses: cc_norm,
        bcc_addresses: bcc_norm,
        from_address: from.to_string(),
    })
}

fn invalid_config(e: lettre::error::Error) -> SmtpOutcome {
    SmtpOutcome::InvalidConfig {
        message: format!("lettre message builder rejected the assembly: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Inputs<'static> {
        // Static lifetime is fine — we feed real owned vecs through clones below.
        Inputs {
            from: "from@example.com".into(),
            to: vec!["to@example.com".into()],
            cc: vec![],
            bcc: vec![],
            subject: "hi".into(),
            body_text: Some("hello".into()),
            body_html: None,
            attachments: &[],
        }
    }

    #[test]
    fn text_only_is_text_plain() {
        let asm = build(base()).unwrap();
        let raw = String::from_utf8(asm.message.formatted()).unwrap();
        assert!(raw.contains("Content-Type: text/plain"));
        assert!(raw.contains("hello"));
        assert!(!raw.contains("multipart/"));
    }

    #[test]
    fn html_only_is_text_html() {
        let mut i = base();
        i.body_text = None;
        i.body_html = Some("<p>hi</p>".into());
        let asm = build(i).unwrap();
        let raw = String::from_utf8(asm.message.formatted()).unwrap();
        assert!(raw.contains("Content-Type: text/html"));
        assert!(raw.contains("<p>hi</p>"));
        assert!(!raw.contains("multipart/"));
    }

    #[test]
    fn text_and_html_is_multipart_alternative() {
        let mut i = base();
        i.body_html = Some("<p>hi</p>".into());
        let asm = build(i).unwrap();
        let raw = String::from_utf8(asm.message.formatted()).unwrap();
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("hello"));
        assert!(raw.contains("<p>hi</p>"));
    }

    #[test]
    fn body_with_attachment_is_multipart_mixed() {
        let attach = LoadedAttachment {
            filename: "report.pdf".into(),
            bytes: b"%PDF-1.4 fake".to_vec(),
            content_type: ContentType::parse("application/pdf").unwrap(),
        };
        let attachments = vec![attach];
        let mut i = base();
        i.attachments = &attachments;
        let asm = build(i).unwrap();
        let raw = String::from_utf8(asm.message.formatted()).unwrap();
        assert!(raw.contains("multipart/mixed"));
        assert!(raw.contains("hello"));
        assert!(raw.contains("report.pdf"));
        assert!(raw.contains("application/pdf"));
    }

    #[test]
    fn parse_named_mailbox() {
        let asm = build(Inputs {
            from: "Reply Bot <bot@example.com>".into(),
            to: vec!["Ada Lovelace <ada@example.com>".into()],
            cc: vec![],
            bcc: vec![],
            subject: "s".into(),
            body_text: Some("b".into()),
            body_html: None,
            attachments: &[],
        })
        .unwrap();
        let raw = String::from_utf8(asm.message.formatted()).unwrap();
        assert!(raw.contains("Reply Bot"));
        assert!(raw.contains("Ada Lovelace"));
    }

    #[test]
    fn bogus_address_returns_invalid_address() {
        let r = build(Inputs {
            from: "from@example.com".into(),
            to: vec!["not a real address".into()],
            cc: vec![],
            bcc: vec![],
            subject: "s".into(),
            body_text: Some("b".into()),
            body_html: None,
            attachments: &[],
        });
        match r {
            Err(SmtpOutcome::InvalidAddress { field, .. }) => assert_eq!(field, "to"),
            _ => panic!("expected InvalidAddress"),
        }
    }

    #[test]
    fn extension_mime_inference() {
        assert_eq!(mime_from_extension(Some("pdf")), "application/pdf");
        assert_eq!(mime_from_extension(Some("PNG")), "image/png");
        assert_eq!(mime_from_extension(Some("unknown")), "application/octet-stream");
        assert_eq!(mime_from_extension(None), "application/octet-stream");
    }
}
