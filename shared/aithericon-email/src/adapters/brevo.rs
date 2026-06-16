//! Brevo (formerly Sendinblue) adapter — posts rendered HTML to the v3
//! transactional email API. Uses `reqwest` (same TLS stack as the rest of the
//! platform); no Brevo SDK dependency.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::Serialize;

use crate::config::BrevoSettings;
use crate::error::{EmailError, Result};
use crate::port::{Mailer, Recipient, TemplateMessage};
use crate::render::Renderer;

const BREVO_ENDPOINT: &str = "https://api.brevo.com/v3/smtp/email";

pub struct BrevoMailer {
    renderer: Arc<Renderer>,
    client: Client,
    api_key: String,
    from: BrevoAddress,
}

#[derive(Debug, Clone, Serialize)]
struct BrevoAddress {
    email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrevoSendRequest<'a> {
    sender: &'a BrevoAddress,
    to: Vec<BrevoAddress>,
    subject: String,
    html_content: String,
}

impl BrevoMailer {
    pub fn new(
        renderer: Arc<Renderer>,
        settings: &BrevoSettings,
        from_address: &str,
        from_name: &str,
    ) -> Result<Self> {
        if settings.api_key.trim().is_empty() {
            return Err(EmailError::Config("brevo api_key is empty".into()));
        }
        let client = Client::builder()
            .build()
            .map_err(|e| EmailError::Config(format!("brevo http client: {e}")))?;
        Ok(Self {
            renderer,
            client,
            api_key: settings.api_key.clone(),
            from: BrevoAddress {
                email: from_address.to_string(),
                name: Some(from_name.to_string()),
            },
        })
    }
}

#[async_trait]
impl Mailer for BrevoMailer {
    async fn send(&self, to: &Recipient, message: &dyn TemplateMessage) -> Result<()> {
        let rendered = self.renderer.render(message)?;

        let body = BrevoSendRequest {
            sender: &self.from,
            to: vec![BrevoAddress {
                email: to.email.clone(),
                name: to.name.clone(),
            }],
            subject: rendered.subject,
            html_content: rendered.html,
        };

        let resp = self
            .client
            .post(BREVO_ENDPOINT)
            .header("api-key", &self.api_key)
            .header("accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| EmailError::Transport(format!("brevo request: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            return Err(EmailError::Transport(format!(
                "brevo returned {status}: {detail}"
            )));
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BrevoSettings;
    use crate::render::Renderer;
    use std::sync::Arc;

    #[test]
    fn empty_api_key_is_rejected() {
        let r = Arc::new(Renderer::new(crate::config::Branding::default()).unwrap());
        let err = BrevoMailer::new(
            r,
            &BrevoSettings {
                api_key: "  ".into(),
            },
            "from@x.test",
            "From",
        );
        assert!(matches!(err, Err(EmailError::Config(_))));
    }

    #[test]
    fn request_serializes_to_brevo_wire_shape() {
        let sender = BrevoAddress {
            email: "from@x.test".into(),
            name: Some("From".into()),
        };
        let body = BrevoSendRequest {
            sender: &sender,
            to: vec![BrevoAddress {
                email: "to@x.test".into(),
                name: None,
            }],
            subject: "Hi".into(),
            html_content: "<p>Hi</p>".into(),
        };
        let v: serde_json::Value = serde_json::to_value(&body).unwrap();
        // Brevo expects camelCase `htmlContent` + sender/to objects.
        assert_eq!(v["htmlContent"], "<p>Hi</p>");
        assert_eq!(v["subject"], "Hi");
        assert_eq!(v["sender"]["email"], "from@x.test");
        assert_eq!(v["sender"]["name"], "From");
        assert_eq!(v["to"][0]["email"], "to@x.test");
        // `name: None` is omitted, not serialized as null.
        assert!(v["to"][0].get("name").is_none());
    }
}
