//! Programmable `TokenVerifier` test double — the whole point of having a
//! trait port. Tests construct a verifier with the exact outcome they want
//! to exercise, swap it into `AppState`, and call the middleware.

use std::collections::BTreeMap;

use async_trait::async_trait;
use mekhan_service::auth::{AuthError, TokenVerifier, VerifiedClaims};

#[derive(Debug, Clone)]
pub enum MockOutcome {
    /// Verifier accepts every token and returns these claims.
    Accept { subject: String, email: Option<String> },
    /// Verifier rejects every token with `InvalidToken`.
    Reject,
    /// Verifier rejects every token with `Expired`.
    Expired,
}

pub struct MockTokenVerifier {
    pub outcome: MockOutcome,
}

impl MockTokenVerifier {
    pub fn accepting(subject: &str) -> Self {
        Self {
            outcome: MockOutcome::Accept {
                subject: subject.to_string(),
                email: Some(format!("{subject}@test")),
            },
        }
    }

    pub fn rejecting() -> Self {
        Self {
            outcome: MockOutcome::Reject,
        }
    }

    pub fn expired() -> Self {
        Self {
            outcome: MockOutcome::Expired,
        }
    }
}

#[async_trait]
impl TokenVerifier for MockTokenVerifier {
    async fn verify(&self, _raw_token: &str) -> Result<VerifiedClaims, AuthError> {
        match &self.outcome {
            MockOutcome::Accept { subject, email } => {
                let mut extra = BTreeMap::new();
                if let Some(e) = email {
                    extra.insert("email".to_string(), serde_json::Value::String(e.clone()));
                }
                Ok(VerifiedClaims {
                    subject: subject.clone(),
                    issuer: "mock".to_string(),
                    audience: vec!["mekhan".to_string()],
                    expires_at: i64::MAX,
                    extra,
                })
            }
            MockOutcome::Reject => Err(AuthError::InvalidToken("mock reject".into())),
            MockOutcome::Expired => Err(AuthError::Expired),
        }
    }
}
