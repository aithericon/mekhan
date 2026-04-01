use reqwest::Client;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum HpiError {
    #[error("HPI request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("HPI returned {status}: {body}")]
    Api { status: u16, body: String },
    #[error("HPI not configured (no API token)")]
    NotConfigured,
}

impl HpiError {
    pub fn status_code(&self) -> u16 {
        match self {
            Self::Api { status, .. } => *status,
            Self::NotConfigured => 503,
            Self::Request(_) => 502,
        }
    }
}

#[derive(Clone)]
pub struct HpiClient {
    http: Client,
    base_url: String,
    token: String,
}

impl HpiClient {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            http: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.token.is_empty()
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{}", self.base_url, path)
    }

    async fn get(&self, path: &str) -> Result<Value, HpiError> {
        if !self.is_configured() {
            return Err(HpiError::NotConfigured);
        }
        let resp = self
            .http
            .get(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await?;
        Self::parse_response(resp).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, HpiError> {
        if !self.is_configured() {
            return Err(HpiError::NotConfigured);
        }
        let resp = self
            .http
            .post(self.url(path))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        Self::parse_response(resp).await
    }

    async fn parse_response(resp: reqwest::Response) -> Result<Value, HpiError> {
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(HpiError::Api { status, body });
        }
        let body = resp.text().await?;
        if body.is_empty() {
            Ok(Value::Null)
        } else {
            Ok(serde_json::from_str(&body).unwrap_or(Value::String(body)))
        }
    }

    /// List tasks. Pass raw query string (e.g. "status=pending&limit=20").
    pub async fn list_tasks(&self, query: &str) -> Result<Value, HpiError> {
        let path = if query.is_empty() {
            "/tasks".to_string()
        } else {
            format!("/tasks?{}", query)
        };
        self.get(&path).await
    }

    /// Get a single task by ID (includes full definition with steps/blocks).
    pub async fn get_task(&self, task_id: &str) -> Result<Value, HpiError> {
        self.get(&format!("/tasks/{}?include_definition=true", task_id))
            .await
    }

    /// Complete a task with form data.
    pub async fn complete_task(&self, task_id: &str, data: Value) -> Result<Value, HpiError> {
        self.post(
            &format!("/tasks/{}/complete", task_id),
            serde_json::json!({ "data": data }),
        )
        .await
    }

    /// Cancel a task (optionally with a reason).
    pub async fn cancel_task(
        &self,
        task_id: &str,
        reason: Option<&str>,
    ) -> Result<Value, HpiError> {
        let body = match reason {
            Some(r) => serde_json::json!({ "reason": r }),
            None => serde_json::json!({}),
        };
        self.post(&format!("/tasks/{}/cancel", task_id), body).await
    }

    /// List processes. Pass raw query string.
    pub async fn list_processes(&self, query: &str) -> Result<Value, HpiError> {
        let path = if query.is_empty() {
            "/processes".to_string()
        } else {
            format!("/processes?{}", query)
        };
        self.get(&path).await
    }

    /// Get a single process by ID.
    pub async fn get_process(&self, process_id: &str) -> Result<Value, HpiError> {
        self.get(&format!("/processes/{}", process_id)).await
    }
}
