//! Structured outcome of an SMTP send.
//!
//! Surfaced into `ExecutionResult.outputs["outcome"]` so the mekhan instance
//! view can render a meaningful failure detail (template render error vs DNS
//! failure vs recipient rejected) instead of a flat error string. The
//! [`SmtpOutcome::reason`] string for each variant is wire-stable — the
//! frontend `SmtpEnvelope.svelte` pattern-matches on it.

use serde::{Deserialize, Serialize};

/// One outcome value. Always serialized — never carries plaintext from the
/// resolved resource (host/port/username are operational; password is not
/// referenced here).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SmtpOutcome {
    /// Message sent (or `dry_run: true` produced a fully assembled but
    /// unsent message). `message_id` is the value lettre returns from the
    /// SMTP server's response; `recipients` is the list actually accepted.
    Success {
        message_id: Option<String>,
        recipients: Vec<String>,
        server_response: Option<String>,
        dry_run: bool,
    },
    /// A template (subject / body / recipient / from) failed to render.
    TemplateRender { file: String, error: String },
    /// `to` / `cc` / `bcc` / `from` rendered to a non-RFC-5322 address.
    InvalidAddress {
        field: String,
        value: String,
        error: String,
    },
    /// Bad config combination (e.g. port=465 with starttls flag mismatch,
    /// no `from` anywhere, attachment file missing, body too large).
    InvalidConfig { message: String },
    /// TCP connect / DNS failure before SMTP dialog began.
    ConnectFailed {
        host: String,
        port: u16,
        error: String,
    },
    /// TLS handshake / negotiation failure.
    TlsFailed { error: String },
    /// SMTP AUTH rejected the credentials.
    AuthFailed { server_response: Option<String> },
    /// SMTP server rejected one or more recipients (5xx on RCPT).
    RecipientRejected {
        failed_recipients: Vec<String>,
        server_response: Option<String>,
    },
    /// Server returned a permanent error (any 5xx that isn't auth /
    /// recipient-specific). Includes the response so it can be debugged.
    ServerError {
        code: Option<u16>,
        server_response: Option<String>,
    },
    /// Connection / send exceeded the run timeout.
    Timeout,
    /// One of the attachments exceeded the soft cap or was missing on disk.
    AttachmentError { filename: String, error: String },
}

impl SmtpOutcome {
    /// Returns true if the outcome is a successful send (including dry-run).
    pub fn is_success(&self) -> bool {
        matches!(self, SmtpOutcome::Success { .. })
    }

    /// Stable wire-name for the failure reason, suitable for filtering and
    /// for the frontend's pattern-match. Returns "success" for successful
    /// sends (including dry-run).
    pub fn reason(&self) -> &'static str {
        match self {
            SmtpOutcome::Success { .. } => "success",
            SmtpOutcome::TemplateRender { .. } => "template_render",
            SmtpOutcome::InvalidAddress { .. } => "invalid_address",
            SmtpOutcome::InvalidConfig { .. } => "invalid_config",
            SmtpOutcome::ConnectFailed { .. } => "connect_failed",
            SmtpOutcome::TlsFailed { .. } => "tls_failed",
            SmtpOutcome::AuthFailed { .. } => "auth_failed",
            SmtpOutcome::RecipientRejected { .. } => "recipient_rejected",
            SmtpOutcome::ServerError { .. } => "server_error",
            SmtpOutcome::Timeout => "timeout",
            SmtpOutcome::AttachmentError { .. } => "attachment_error",
        }
    }
}

/// Map a `lettre::transport::smtp::Error` onto the closest structured
/// outcome variant. Lettre's error type is opaque in some cases — when the
/// classification is unclear we keep the original text under
/// `SmtpOutcome::ServerError` so operators see the raw SMTP response.
pub fn classify_smtp_error(err: &lettre::transport::smtp::Error) -> SmtpOutcome {
    let msg = err.to_string();

    if err.is_timeout() {
        return SmtpOutcome::Timeout;
    }
    if err.is_tls() {
        return SmtpOutcome::TlsFailed { error: msg };
    }

    // Auth + recipient + generic server errors come through as
    // permanent/transient response errors. We inspect the message text since
    // lettre exposes the response only via Display.
    if let Some(code) = extract_response_code(&msg) {
        if (500..=599).contains(&code) {
            // AUTH failures: 535 "authentication failed" (rfc 4954).
            if code == 535 || msg.to_lowercase().contains("auth") {
                return SmtpOutcome::AuthFailed {
                    server_response: Some(msg),
                };
            }
            // RCPT-specific: 550 "5.1.1 user unknown" etc.
            if code == 550 || code == 551 || code == 553 {
                return SmtpOutcome::RecipientRejected {
                    failed_recipients: vec![],
                    server_response: Some(msg),
                };
            }
            return SmtpOutcome::ServerError {
                code: Some(code),
                server_response: Some(msg),
            };
        }
        return SmtpOutcome::ServerError {
            code: Some(code),
            server_response: Some(msg),
        };
    }

    // Errors that come from before the SMTP dialog completed (DNS / TCP /
    // first byte) are connect failures.
    if err.is_response() {
        return SmtpOutcome::ServerError {
            code: None,
            server_response: Some(msg),
        };
    }

    SmtpOutcome::ConnectFailed {
        host: String::new(),
        port: 0,
        error: msg,
    }
}

/// Parse a leading 3-digit SMTP response code out of an error message — best
/// effort. Lettre concatenates the dialog into the Display string.
fn extract_response_code(msg: &str) -> Option<u16> {
    let bytes = msg.as_bytes();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if bytes[i].is_ascii_digit()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
        {
            // Boundary: previous byte (if any) is non-digit AND next byte (if
            // any) is non-digit. Avoids matching the middle of '1234'.
            let left_ok = i == 0 || !bytes[i - 1].is_ascii_digit();
            let right_ok = i + 3 == bytes.len() || !bytes[i + 3].is_ascii_digit();
            if left_ok && right_ok {
                return std::str::from_utf8(&bytes[i..i + 3])
                    .ok()
                    .and_then(|s| s.parse::<u16>().ok());
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_is_stable_for_renderer_dispatch() {
        // These string values are de-facto wire contract — the frontend
        // `SmtpEnvelope.svelte` pattern-matches on `outcome.reason`. If you
        // need to change one, plan the renderer change in lockstep.
        assert_eq!(
            SmtpOutcome::Success {
                message_id: None,
                recipients: vec![],
                server_response: None,
                dry_run: false,
            }
            .reason(),
            "success"
        );
        assert_eq!(
            SmtpOutcome::TemplateRender {
                file: "x".into(),
                error: "y".into(),
            }
            .reason(),
            "template_render"
        );
        assert_eq!(
            SmtpOutcome::ConnectFailed {
                host: "h".into(),
                port: 587,
                error: "e".into(),
            }
            .reason(),
            "connect_failed"
        );
    }

    #[test]
    fn extract_response_code_finds_codes_with_boundaries() {
        assert_eq!(
            extract_response_code("535 5.7.8 authentication failed"),
            Some(535)
        );
        assert_eq!(
            extract_response_code("permanent failure: 550 user unknown"),
            Some(550)
        );
        // 4-digit number is bounded by digits on at least one side at every
        // candidate start, so we extract nothing.
        assert_eq!(extract_response_code("port 1025 connection refused"), None);
        // Embedded multi-digit: no isolated 3-digit substring → None
        assert_eq!(extract_response_code("12345"), None);
    }
}
