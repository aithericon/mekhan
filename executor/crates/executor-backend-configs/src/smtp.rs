use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use aithericon_executor_domain::{ExecutionSpec, ExecutorError, InputDeclaration, OutputDeclaration};

/// Configuration for the SMTP backend.
///
/// The backend receives this via `ExecutionSpec.config`. Recipient strings,
/// the subject line, the body sources, and the optional `from` override
/// are Tera templates rendered against a context built from staged input
/// files (`<slug>.json`) and the resolved `smtp` resource view.
///
/// The mekhan compiler **embeds the template source** directly in this
/// config (read from the per-node Yjs files at publish time). This keeps the
/// executor stateless about the editor's node-file storage and avoids a
/// second I/O path for template lookup. Attachments do go through the normal
/// `inputs[]` staging pipeline though — those are typically larger and
/// reference upstream-step outputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpConfig {
    /// To addresses. Each entry is a Tera template; rendered values must
    /// be RFC 5322 addresses. At least one recipient (To + Cc + Bcc combined)
    /// is required.
    #[serde(default)]
    pub to: Vec<String>,

    /// Cc addresses.
    #[serde(default)]
    pub cc: Vec<String>,

    /// Bcc addresses.
    #[serde(default)]
    pub bcc: Vec<String>,

    /// Optional From override. If absent, falls back to the SMTP resource's
    /// `from_address` field. If both are absent, validation fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,

    /// Subject template (Tera source).
    pub subject: TemplateSource,

    /// Plain-text body template (Tera source). Optional but at least one of
    /// `body_text` / `body_html` must be set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_text: Option<TemplateSource>,

    /// HTML body template (Tera source). When both `body_text` and
    /// `body_html` are set, the message is sent multipart/alternative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_html: Option<TemplateSource>,

    /// Attachments. Each entry references a staged input by `input_name`
    /// (which the compiler synthesizes as `"_att_<idx>"`); `filename` is the
    /// name the recipient sees and `mime` overrides the auto-detected type.
    #[serde(default)]
    pub attachments: Vec<AttachmentSpec>,

    /// Resource alias inside the workflow — used by the compiler to look up
    /// the right SMTP resource binding. Echoed here for diagnostics and to
    /// drive the template-context variable name; the backend reads transport
    /// config from `resolved_config["smtp_resource"]`, not from this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_alias: Option<String>,

    /// When true, render templates + assemble MIME but do not connect to the
    /// SMTP server. Outputs include the rendered subject/body for inspection.
    #[serde(default)]
    pub dry_run: bool,

    /// Optional extra string fields surfaced into the Tera context under
    /// `vars.<key>`. Useful for static per-template constants (signing-off
    /// name, support URL, …) the workflow author doesn't want to clutter
    /// upstream node outputs with.
    #[serde(default)]
    pub vars: HashMap<String, String>,
}

/// One template source. Carries the source bytes inline plus a label used
/// for diagnostic messages ("error in subject.tera at line 3"). The label
/// is the original node-file name from the editor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateSource {
    /// Display name — typically the original node-file name (`subject.tera`).
    pub label: String,
    /// Raw Tera template text.
    pub source: String,
}

impl TemplateSource {
    pub fn new(label: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            source: source.into(),
        }
    }
}

/// One attachment. The compiler emits one of these per `attachments[]` entry
/// in the workflow config and pairs it with a synthesized `inputs[]` entry
/// that materializes the file into the run's staged-inputs directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentSpec {
    /// Filename the recipient sees in the mail client.
    pub filename: String,
    /// Staged-input name where the bytes live (one of `RunContext.staged_inputs`).
    pub input_name: String,
    /// Optional MIME override; otherwise inferred from `filename` extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

impl SmtpConfig {
    pub fn into_spec(self) -> ExecutionSpec {
        self.into_spec_with_io(vec![], vec![])
    }

    pub fn into_spec_with_io(
        self,
        inputs: Vec<InputDeclaration>,
        outputs: Vec<OutputDeclaration>,
    ) -> ExecutionSpec {
        ExecutionSpec {
            backend: "smtp".into(),
            inputs,
            outputs,
            config: serde_json::to_value(self).expect("SmtpConfig serialization cannot fail"),
            config_ref: None,
        }
    }

    pub fn from_spec(spec: &ExecutionSpec) -> Result<Self, ExecutorError> {
        serde_json::from_value(spec.config.clone())
            .map_err(|e| ExecutorError::Config(format!("invalid smtp backend config: {e}")))
    }

    /// Validate independent of the resolved resource. Recipient counts,
    /// body-source presence, attachment name shape.
    pub fn validate(&self) -> Result<(), ExecutorError> {
        if self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty() {
            return Err(ExecutorError::Config(
                "smtp config: at least one recipient (to / cc / bcc) is required".into(),
            ));
        }
        if self.subject.source.trim().is_empty() {
            return Err(ExecutorError::Config(
                "smtp config: subject template source is required".into(),
            ));
        }
        if self.body_text.is_none() && self.body_html.is_none() {
            return Err(ExecutorError::Config(
                "smtp config: at least one of body_text or body_html is required".into(),
            ));
        }
        for a in &self.attachments {
            if a.filename.trim().is_empty() {
                return Err(ExecutorError::Config(
                    "smtp config: attachment filename must be non-empty".into(),
                ));
            }
            if a.input_name.trim().is_empty() {
                return Err(ExecutorError::Config(
                    "smtp config: attachment input_name must be non-empty".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Resolved SMTP resource binding parked in `RunContext.resolved_config` by
/// the launcher's `ResourceResolver`. The structure mirrors
/// `aithericon_resources::types::Smtp` exactly so the mekhan side and the
/// backend stay in lockstep without a dep edge between them.
///
/// `password` is intentionally `String` and not redacted — the resolved-config
/// side-channel is `#[serde(skip)]`, never serialized to disk, never logged
/// via the elision-Debug impl on `RunContext`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedSmtpResource {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_address: Option<String>,
}

impl ResolvedSmtpResource {
    /// Extract from a `resolved_config` JSON blob. The launcher writes the
    /// resource under the `smtp_resource` key — keep this in one place so
    /// the executor and the service-side compiler agree.
    pub const RESOLVED_CONFIG_KEY: &'static str = "smtp_resource";

    pub fn from_resolved(resolved: &serde_json::Value) -> Result<Self, ExecutorError> {
        let v = resolved
            .get(Self::RESOLVED_CONFIG_KEY)
            .ok_or_else(|| {
                ExecutorError::Config(format!(
                    "smtp backend: resolved_config missing '{}' key",
                    Self::RESOLVED_CONFIG_KEY
                ))
            })?
            .clone();
        Self::from_resolved_value(&v)
    }

    /// Deserialize directly from a JSON object whose keys are the resource
    /// fields (host, port, username, password, from_address) — used when the
    /// envelope is read out of a staged `<alias>.json` file rather than
    /// nested under the `smtp_resource` side-channel key.
    pub fn from_resolved_value(v: &serde_json::Value) -> Result<Self, ExecutorError> {
        serde_json::from_value(v.clone()).map_err(|e| {
            ExecutorError::Config(format!("smtp backend: invalid resolved resource shape: {e}"))
        })
    }
}
