//! Webhook trigger source (Phase 5e).
//!
//! Exposes `POST /api/triggers/webhook/{slug}` (no auth middleware — the
//! handler does its own auth based on `WebhookTrigger.auth`). The receiver
//! resolves the slug to a registered trigger, validates auth, and fires the
//! trigger with a payload of `{ payload (body), headers, query, fire_time }`.
//!
//! Slugs are template-scoped (proposal §9.4 — they should survive version
//! supersede), so the dispatcher serves the latest published version's
//! trigger whenever multiple match.

use std::collections::HashMap;

use crate::models::template::{TriggerSource, WebhookAuth, WebhookTrigger};
use crate::triggers::dispatcher::TriggerDispatcher;
use crate::triggers::model::TriggerRecord;

/// Resolve a registered Webhook trigger by its slug. If multiple triggers
/// share the same slug (different template versions), returns the one with
/// the highest `template_version` so external URLs keep working across
/// supersede.
pub fn find_by_slug(dispatcher: &TriggerDispatcher, slug: &str) -> Option<TriggerRecord> {
    let mut best: Option<TriggerRecord> = None;
    for rec in dispatcher.list_all() {
        let TriggerSource::Webhook(ref w) = rec.source else {
            continue;
        };
        if w.slug != slug {
            continue;
        }
        match &best {
            None => best = Some(rec),
            Some(prev) if rec.template_version > prev.template_version => best = Some(rec),
            _ => {}
        }
    }
    best
}

/// Verify the webhook auth scheme against the supplied headers / body. Returns
/// `Ok(())` if the request is allowed to proceed, otherwise an error string
/// suitable for a 401 response.
pub fn check_auth(
    trigger: &WebhookTrigger,
    headers: &HashMap<String, String>,
    body: &[u8],
    secret_resolver: impl FnOnce(&str) -> Option<String>,
) -> Result<(), String> {
    match &trigger.auth {
        WebhookAuth::None => Ok(()),
        WebhookAuth::SharedSecret { header, secret_ref } => {
            let provided = lookup_header(headers, header).ok_or_else(|| {
                format!("missing required auth header '{header}'")
            })?;
            let expected = secret_resolver(secret_ref).ok_or_else(|| {
                format!("secret '{secret_ref}' not configured")
            })?;
            if constant_eq(provided.as_bytes(), expected.as_bytes()) {
                Ok(())
            } else {
                Err("auth header value mismatch".to_string())
            }
        }
        WebhookAuth::SignedHmac { header, secret_ref } => {
            // Phase 5e ships SharedSecret; SignedHmac validation is stubbed
            // out until we wire a real signing helper. Reject with a clear
            // message so test traffic doesn't silently slip through.
            let _ = (header, secret_ref, body);
            Err("SignedHmac webhook auth is not yet implemented (Phase 5e ships SharedSecret only)"
                .to_string())
        }
    }
}

fn lookup_header(headers: &HashMap<String, String>, key: &str) -> Option<String> {
    // Case-insensitive header lookup (HTTP header names are case-insensitive).
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

/// Constant-time equality check. Required for secret comparisons so an
/// attacker can't time-side-channel the secret value byte by byte.
fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(header: &str, secret_ref: &str) -> WebhookTrigger {
        WebhookTrigger {
            slug: "test".to_string(),
            auth: WebhookAuth::SharedSecret {
                header: header.to_string(),
                secret_ref: secret_ref.to_string(),
            },
            require_method: None,
        }
    }

    #[test]
    fn none_auth_always_passes() {
        let t = WebhookTrigger {
            slug: "x".to_string(),
            auth: WebhookAuth::None,
            require_method: None,
        };
        assert!(check_auth(&t, &HashMap::new(), &[], |_| None).is_ok());
    }

    #[test]
    fn shared_secret_matches() {
        let t = shared("X-Token", "test_secret");
        let mut h = HashMap::new();
        h.insert("X-Token".to_string(), "abc123".to_string());
        let ok = check_auth(&t, &h, &[], |k| {
            if k == "test_secret" {
                Some("abc123".to_string())
            } else {
                None
            }
        });
        assert!(ok.is_ok());
    }

    #[test]
    fn shared_secret_rejects_on_mismatch() {
        let t = shared("X-Token", "test_secret");
        let mut h = HashMap::new();
        h.insert("X-Token".to_string(), "wrong".to_string());
        let err = check_auth(&t, &h, &[], |_| Some("abc123".to_string()));
        assert!(err.is_err());
    }

    #[test]
    fn shared_secret_rejects_missing_header() {
        let t = shared("X-Token", "test_secret");
        let err = check_auth(&t, &HashMap::new(), &[], |_| Some("x".to_string()));
        assert!(err.is_err());
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let t = shared("Authorization", "s");
        let mut h = HashMap::new();
        h.insert("authorization".to_string(), "x".to_string());
        let ok = check_auth(&t, &h, &[], |_| Some("x".to_string()));
        assert!(ok.is_ok());
    }

    #[test]
    fn signed_hmac_is_not_yet_implemented() {
        let t = WebhookTrigger {
            slug: "x".to_string(),
            auth: WebhookAuth::SignedHmac {
                header: "X-Sig".to_string(),
                secret_ref: "k".to_string(),
            },
            require_method: None,
        };
        let err = check_auth(&t, &HashMap::new(), &[], |_| None);
        assert!(err.is_err());
    }
}
