//! Dev-mode adapter: `NoopTokenVerifier` returns a fixed user regardless of
//! input. Selected when `auth.mode = "dev_noop"`. Never used in production —
//! `main.rs` is responsible for refusing to boot if dev mode is paired with a
//! prod environment.

use async_trait::async_trait;

use super::model::{AuthError, VerifiedClaims};
use super::port::TokenVerifier;

#[derive(Debug, Clone)]
pub struct NoopTokenVerifier {
    subject: String,
    email: Option<String>,
    display_name: Option<String>,
}

impl Default for NoopTokenVerifier {
    fn default() -> Self {
        Self {
            subject: "dev-user".to_string(),
            email: Some("dev@local".to_string()),
            display_name: Some("Dev User".to_string()),
        }
    }
}

#[async_trait]
impl TokenVerifier for NoopTokenVerifier {
    async fn verify(&self, _raw_token: &str) -> Result<VerifiedClaims, AuthError> {
        let mut extra = std::collections::BTreeMap::new();
        if let Some(email) = &self.email {
            extra.insert("email".to_string(), serde_json::Value::String(email.clone()));
        }
        if let Some(name) = &self.display_name {
            extra.insert("name".to_string(), serde_json::Value::String(name.clone()));
        }
        Ok(VerifiedClaims {
            subject: self.subject.clone(),
            issuer: "dev-noop".to_string(),
            audience: vec!["dev".to_string()],
            expires_at: i64::MAX,
            extra,
        })
    }
}
