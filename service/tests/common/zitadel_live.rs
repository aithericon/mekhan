//! Live Zitadel helper for the gated Layer-2 e2e test.
//!
//! Mints/revokes a service-user Personal Access Token via the Zitadel
//! management API (authenticated with the FirstInstance admin PAT that the
//! `zitadel` container writes to `deploy/zitadel/pat/`), and reads the
//! introspection API-app credentials that `bootstrap.sh` wrote into
//! `mekhan.local.toml`. Inert unless `MEKHAN_E2E_ZITADEL=1`.

#![allow(dead_code)] // only the zitadel_e2e crate uses this

use serde_json::{json, Value};

pub struct LiveZitadel {
    base: String,
    admin_pat: String,
    http: reqwest::Client,
}

/// Introspection credentials bootstrap.sh wrote (issuer, client_id, secret).
pub struct IntrospectCreds {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
}

impl LiveZitadel {
    /// `None` unless `MEKHAN_E2E_ZITADEL=1`. Reads the admin PAT from
    /// `MEKHAN_E2E_ADMIN_PAT_FILE` (default `../deploy/zitadel/pat/
    /// zitadel-admin-sa.pat`, relative to the `service/` test CWD).
    pub fn from_env() -> Option<Self> {
        if std::env::var("MEKHAN_E2E_ZITADEL").ok().as_deref() != Some("1") {
            return None;
        }
        let base = std::env::var("MEKHAN_E2E_ISSUER")
            .unwrap_or_else(|_| "http://localhost:8080".to_string());
        let pat_file = std::env::var("MEKHAN_E2E_ADMIN_PAT_FILE")
            .unwrap_or_else(|_| "../deploy/zitadel/pat/zitadel-admin-sa.pat".to_string());
        let admin_pat = std::fs::read_to_string(&pat_file)
            .unwrap_or_else(|e| panic!("read admin PAT {pat_file}: {e}"))
            .trim()
            .to_string();
        Some(Self {
            base,
            admin_pat,
            http: reqwest::Client::new(),
        })
    }

    /// Read issuer + introspection API-app creds from the bootstrap-written
    /// `mekhan.local.toml` (CWD `service/`, then repo root).
    pub fn introspection_creds(&self) -> IntrospectCreds {
        let toml = std::fs::read_to_string("mekhan.local.toml")
            .or_else(|_| std::fs::read_to_string("../mekhan.local.toml"))
            .expect("mekhan.local.toml not found — run deploy/zitadel/bootstrap.sh");
        let get = |k: &str| scan_toml(&toml, k);
        IntrospectCreds {
            issuer: get("issuer_url").expect("issuer_url in mekhan.local.toml"),
            client_id: get("introspection_client_id")
                .expect("introspection_client_id — re-run bootstrap.sh after the API-app change"),
            client_secret: get("introspection_client_secret")
                .expect("introspection_client_secret — re-run bootstrap.sh"),
        }
    }

    /// Issuer URL bootstrap.sh wrote (host of the live Zitadel).
    pub fn issuer(&self) -> String {
        self.base.clone()
    }

    /// The `mekhan-token-broker` PAT bootstrap.sh wrote — drives the embedded
    /// `/api/auth/tokens` broker against live Zitadel.
    pub fn broker_pat(&self) -> String {
        let toml = std::fs::read_to_string("mekhan.local.toml")
            .or_else(|_| std::fs::read_to_string("../mekhan.local.toml"))
            .expect("mekhan.local.toml not found — run deploy/zitadel/bootstrap.sh");
        scan_toml(&toml, "broker_pat")
            .expect("broker_pat in mekhan.local.toml — re-run bootstrap.sh after the broker change")
    }

    async fn mgmt(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let mut rb = self
            .http
            .request(method, format!("{}{}", self.base, path))
            .bearer_auth(&self.admin_pat);
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let resp = rb.send().await.expect("zitadel mgmt request");
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "zitadel {path} -> {status}: {text}"
        );
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    /// Idempotently ensure a machine (service) user exists; returns its id.
    pub async fn ensure_service_user(&self, username: &str) -> String {
        let found = self
            .mgmt(
                reqwest::Method::POST,
                "/v2/users",
                Some(json!({"queries":[{"userNameQuery":{"userName":username,
                    "method":"TEXT_QUERY_METHOD_EQUALS"}}]})),
            )
            .await;
        if let Some(id) = found["result"][0]["userId"].as_str() {
            return id.to_string();
        }
        let created = self
            .mgmt(
                reqwest::Method::POST,
                "/management/v1/users/machine",
                Some(json!({"userName":username,"name":username,
                    "description":"mekhan introspection e2e"})),
            )
            .await;
        created["userId"]
            .as_str()
            .expect("created machine userId")
            .to_string()
    }

    /// Mint a PAT for the service user → `(token_id, token)`.
    pub async fn mint_pat(&self, user_id: &str) -> (String, String) {
        let r = self
            .mgmt(
                reqwest::Method::POST,
                &format!("/management/v1/users/{user_id}/pats"),
                Some(json!({})),
            )
            .await;
        (
            r["tokenId"].as_str().expect("tokenId").to_string(),
            r["token"].as_str().expect("token").to_string(),
        )
    }

    pub async fn revoke_pat(&self, user_id: &str, token_id: &str) {
        self.mgmt(
            reqwest::Method::DELETE,
            &format!("/management/v1/users/{user_id}/pats/{token_id}"),
            None,
        )
        .await;
    }
}

/// Minimal `key = "value"` scan — avoids a `toml` dev-dependency for three
/// well-known keys in a generated file.
fn scan_toml(src: &str, key: &str) -> Option<String> {
    for line in src.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                return Some(rest.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}
