//! SMTP executor backend with Tera-templated subject + body.
//!
//! The backend is stateless and side-effect-free until [`SmtpBackend::execute`]
//! runs — it pulls the resolved SMTP resource (host/port/auth) out of
//! `RunContext.resolved_config` (the `#[serde(skip)]` side-channel populated
//! by the worker's `PlanSecretsHook`), renders Tera templates against the
//! staged input files, builds a MIME message, and dispatches via
//! `lettre::AsyncSmtpTransport`. Failures are mapped to a structured
//! [`outcome::SmtpOutcome`] so the mekhan instance view can render a
//! meaningful detail (template render error vs DNS failure vs recipient
//! rejected) instead of a flat error string.

pub mod multipart;
pub mod outcome;
pub mod template;
pub mod transport;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lettre::AsyncTransport;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use aithericon_executor_backend::traits::{EventStream, ExecutionBackend, StatusCallback};
use aithericon_executor_backend_configs::smtp::{
    ResolvedSmtpResource, SmtpConfig, TemplateSource,
};
use aithericon_executor_domain::{
    ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError, RunContext,
};

use crate::outcome::SmtpOutcome;

/// Optional dependency for tests / harnesses that want to capture sent
/// messages without a real SMTP server. When set, the backend skips the
/// transport build and reports success with the rendered payload echoed
/// into the outcome.
///
/// Production wiring (`executor-service::main::build_executor`) never sets
/// this — it's a unit-test seam that conformance tests can rely on.
pub trait MessageSink: Send + Sync {
    fn accept(&self, msg: &lettre::Message);
}

/// `SmtpBackend` is `supports(spec) == spec.backend == "smtp"`.
pub struct SmtpBackend {
    sink: Option<Arc<dyn MessageSink>>,
}

impl SmtpBackend {
    pub fn new() -> Self {
        Self { sink: None }
    }

    /// Replace the network send with a `MessageSink` — for unit tests that
    /// want to assert MIME shape without a real server. Sets `dry_run`
    /// semantics implicitly (no transport built) but still verifies
    /// template render + assembly succeed.
    pub fn with_sink(mut self, sink: Arc<dyn MessageSink>) -> Self {
        self.sink = Some(sink);
        self
    }
}

impl Default for SmtpBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionBackend for SmtpBackend {
    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        _event_stream: Option<Arc<dyn EventStream>>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let start = tokio::time::Instant::now();
        let config = SmtpConfig::from_spec(&run_context.spec)?;
        config.validate()?;

        // The resolved resource rides through the staged-inputs pipeline as
        // `<resource_alias>.json` — same channel the Python runner uses for
        // its resource borrows. `PlanSecretsHook` walks inline input values
        // at staging time, substituting `{{secret:resources/<id>/v<n>#field}}`
        // patterns with the live Vault values, so by the time we read the
        // file here it has plaintext credentials. Path of last resort:
        // `resolved_config["smtp_resource"]` (kept for harness tests that
        // don't go through the full staging pipeline).
        let resource = match resolve_resource(&config, run_context) {
            Ok(r) => r,
            Err(out) => return Ok(failure_result(out, start.elapsed(), run_context)),
        };

        // Build the Tera context. Reads every <slug>.json under inputs_dir.
        let tera_ctx = match template::build_context(
            &run_context.run_dir.inputs_dir,
            config.resource_alias.as_deref(),
            &resource,
            &config.vars,
            &run_context.execution_id,
        ) {
            Ok(c) => c,
            Err(e) => {
                return Ok(failure_result(
                    SmtpOutcome::InvalidConfig {
                        message: e.to_string(),
                    },
                    start.elapsed(),
                    run_context,
                ));
            }
        };

        // Render subject and bodies. Each render failure short-circuits.
        let subject = match render_one(&config.subject, &tera_ctx) {
            Ok(s) => s,
            Err(out) => return Ok(failure_result(out, start.elapsed(), run_context)),
        };
        let body_text = match config.body_text.as_ref().map(|t| render_one(t, &tera_ctx)).transpose() {
            Ok(v) => v,
            Err(out) => return Ok(failure_result(out, start.elapsed(), run_context)),
        };
        let body_html = match config.body_html.as_ref().map(|t| render_one(t, &tera_ctx)).transpose() {
            Ok(v) => v,
            Err(out) => return Ok(failure_result(out, start.elapsed(), run_context)),
        };

        // Render recipients. Each entry is its own template — we render with
        // an anonymous label so the error points at "to[0]" vs "cc[2]".
        let to = match render_addr_list(&config.to, &tera_ctx, "to") {
            Ok(v) => v,
            Err(out) => return Ok(failure_result(out, start.elapsed(), run_context)),
        };
        let cc = match render_addr_list(&config.cc, &tera_ctx, "cc") {
            Ok(v) => v,
            Err(out) => return Ok(failure_result(out, start.elapsed(), run_context)),
        };
        let bcc = match render_addr_list(&config.bcc, &tera_ctx, "bcc") {
            Ok(v) => v,
            Err(out) => return Ok(failure_result(out, start.elapsed(), run_context)),
        };

        // Resolve From: config override → resource.from_address → error.
        let from = match resolve_from(&config, &resource, &tera_ctx) {
            Ok(v) => v,
            Err(out) => return Ok(failure_result(out, start.elapsed(), run_context)),
        };

        // Load attachments + parse content types.
        let attachments = match multipart::load_attachments(&config.attachments, &run_context.staged_inputs) {
            Ok(v) => v,
            Err(out) => return Ok(failure_result(out, start.elapsed(), run_context)),
        };

        let assembled = match multipart::build(multipart::Inputs {
            from,
            to,
            cc,
            bcc,
            subject: subject.clone(),
            body_text: body_text.clone(),
            body_html: body_html.clone(),
            attachments: &attachments,
        }) {
            Ok(m) => m,
            Err(out) => return Ok(failure_result(out, start.elapsed(), run_context)),
        };

        // dry_run + sink (test seam) skip the network — same code path.
        if config.dry_run || self.sink.is_some() {
            if let Some(sink) = &self.sink {
                sink.accept(&assembled.message);
            }
            let mut recipients = assembled.to_addresses.clone();
            recipients.extend(assembled.cc_addresses.clone());
            recipients.extend(assembled.bcc_addresses.clone());
            return Ok(success_result(
                SmtpOutcome::Success {
                    message_id: None,
                    recipients,
                    server_response: None,
                    dry_run: true,
                },
                subject,
                body_text,
                body_html,
                start.elapsed(),
                run_context,
            ));
        }

        status_cb(
            ExecutionStatus::Running,
            serde_json::json!({
                "host": resource.host,
                "port": resource.port,
                "to_count": assembled.to_addresses.len(),
                "cc_count": assembled.cc_addresses.len(),
                "bcc_count": assembled.bcc_addresses.len(),
                "has_text_body": body_text.is_some(),
                "has_html_body": body_html.is_some(),
                "attachment_count": attachments.len(),
            }),
        )
        .await;

        // Build the transport. Port-to-mode dispatch lives in `transport`.
        let transport = match transport::build(&resource) {
            transport::BuildResult::Ready(t) => t,
            transport::BuildResult::Invalid(out) => {
                return Ok(failure_result(out, start.elapsed(), run_context))
            }
        };

        let timeout = if run_context.timeout > Duration::ZERO {
            run_context.timeout
        } else {
            Duration::from_secs(60)
        };

        tokio::select! { biased;
            _ = cancel.cancelled() => {
                info!("smtp send cancelled");
                Ok(ExecutionResult {
                    outcome: ExecutionOutcome::Cancelled,
                    duration: start.elapsed(),
                    stdout_tail: None,
                    stderr_tail: None,
                    artifact_manifest: None,
                    outputs: HashMap::new(),
                    progress: None,
                    run_dir: Some(run_context.run_dir.clone()),
                    metrics: None,
                    logs: None,
                })
            }
            _ = tokio::time::sleep(timeout) => {
                warn!(timeout_secs = timeout.as_secs(), "smtp send timed out");
                Ok(failure_result(SmtpOutcome::Timeout, start.elapsed(), run_context))
            }
            result = transport.send(assembled.message) => {
                let duration = start.elapsed();
                match result {
                    Ok(response) => {
                        debug!(?response, "smtp send accepted");
                        let mut recipients = assembled.to_addresses.clone();
                        recipients.extend(assembled.cc_addresses.clone());
                        recipients.extend(assembled.bcc_addresses.clone());
                        Ok(success_result(
                            SmtpOutcome::Success {
                                message_id: response.first_line().map(|s| s.to_string()),
                                recipients,
                                server_response: response.message().next().map(|s| s.to_string()),
                                dry_run: false,
                            },
                            subject,
                            body_text,
                            body_html,
                            duration,
                            run_context,
                        ))
                    }
                    Err(e) => {
                        warn!(error = %e, "smtp send failed");
                        let mut out = outcome::classify_smtp_error(&e);
                        // Fill the connect-failure host/port we didn't know about
                        // inside the classifier.
                        if let SmtpOutcome::ConnectFailed { host, port, .. } = &mut out {
                            *host = resource.host.clone();
                            *port = resource.port;
                        }
                        Ok(failure_result(out, duration, run_context))
                    }
                }
            }
        }
    }

    fn name(&self) -> &'static str {
        "smtp"
    }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == "smtp"
    }
}

/// Resolve the SMTP resource view from the staging pipeline.
///
/// Preferred path: the compiler emits a `BorrowResolution::ResourceEnvelope`
/// for the SMTP step's `resource_alias`, which stages `<alias>.json` into
/// `run_dir/inputs/`. `PlanSecretsHook` walks inline input contents at
/// staging time so the file has plaintext secrets by the time we read it.
///
/// Fallback path: `resolved_config["smtp_resource"]` — used by unit tests
/// that construct a `RunContext` directly with a pre-resolved resource
/// rather than going through the full staging pipeline.
fn resolve_resource(
    config: &SmtpConfig,
    run_context: &RunContext,
) -> Result<ResolvedSmtpResource, SmtpOutcome> {
    if let Some(alias) = &config.resource_alias {
        if !alias.is_empty() {
            // Shared loader: handles staged_inputs → inputs_dir fallback,
            // returns Ok(None) when the file isn't present (we fall
            // through to the test-harness `resolved_config` path below).
            let envelope = aithericon_executor_backend::try_load_resource_envelope(
                run_context, alias,
            )
            .map_err(|e| SmtpOutcome::InvalidConfig {
                message: format!("smtp backend: {e}"),
            })?;
            if let Some(value) = envelope {
                return ResolvedSmtpResource::from_resolved_value(&value).map_err(|e| {
                    SmtpOutcome::InvalidConfig {
                        message: format!("smtp backend: {e}"),
                    }
                });
            }
        }
    }

    if let Some(rc) = run_context.resolved_config.as_ref() {
        return ResolvedSmtpResource::from_resolved(rc).map_err(|e| SmtpOutcome::InvalidConfig {
            message: format!("smtp backend: {e}"),
        });
    }

    Err(SmtpOutcome::InvalidConfig {
        message: format!(
            "smtp backend: resource '{}' not staged as <alias>.json and no resolved_config — \
             the compiler must emit a ResourceEnvelope borrow OR a test harness must \
             populate resolved_config['smtp_resource']",
            config.resource_alias.as_deref().unwrap_or("<unset>")
        ),
    })
}

/// Render one template + propagate the outcome on failure.
fn render_one(t: &TemplateSource, ctx: &tera::Context) -> Result<String, SmtpOutcome> {
    template::render(&t.source, ctx, &t.label)
}

fn render_addr_list(
    list: &[String],
    ctx: &tera::Context,
    which: &str,
) -> Result<Vec<String>, SmtpOutcome> {
    let mut out = Vec::with_capacity(list.len());
    for (i, src) in list.iter().enumerate() {
        let label = format!("{which}[{i}]");
        out.push(template::render(src, ctx, &label)?);
    }
    Ok(out)
}

fn resolve_from(
    config: &SmtpConfig,
    resource: &ResolvedSmtpResource,
    ctx: &tera::Context,
) -> Result<String, SmtpOutcome> {
    if let Some(src) = &config.from {
        return template::render(src, ctx, "from");
    }
    if let Some(default) = &resource.from_address {
        return Ok(default.clone());
    }
    Err(SmtpOutcome::InvalidConfig {
        message: "smtp config: `from` is not set and the bound SMTP resource has no \
                  `from_address` default — set one on the resource or override per-step"
            .into(),
    })
}

/// Wrap a successful outcome in an [`ExecutionResult`]. Includes the rendered
/// subject + body previews so the instance-view renderer can show them without
/// re-rendering.
fn success_result(
    outcome: SmtpOutcome,
    subject: String,
    body_text: Option<String>,
    body_html: Option<String>,
    duration: Duration,
    run_context: &RunContext,
) -> ExecutionResult {
    let mut outputs = HashMap::new();
    outputs.insert(
        "outcome".into(),
        serde_json::to_value(&outcome).expect("SmtpOutcome serializes"),
    );
    outputs.insert("subject".into(), serde_json::Value::String(subject));
    if let Some(t) = body_text {
        outputs.insert(
            "body_text_preview".into(),
            serde_json::Value::String(truncate_preview(&t)),
        );
    }
    if let Some(h) = body_html {
        outputs.insert(
            "body_html_preview".into(),
            serde_json::Value::String(truncate_preview(&h)),
        );
    }
    ExecutionResult {
        outcome: ExecutionOutcome::Success,
        duration,
        stdout_tail: None,
        stderr_tail: None,
        artifact_manifest: None,
        outputs,
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

/// Wrap a failure outcome in an [`ExecutionResult`]. Returns
/// `ExecutionOutcome::BackendError` so the engine surfaces it as a Failed
/// status; the structured detail is in `outputs["outcome"]`.
fn failure_result(
    outcome: SmtpOutcome,
    duration: Duration,
    run_context: &RunContext,
) -> ExecutionResult {
    let detail = serde_json::to_value(&outcome).expect("SmtpOutcome serializes");
    let summary = format!("smtp: {}", outcome.reason());
    let mut outputs = HashMap::new();
    outputs.insert("outcome".into(), detail);
    ExecutionResult {
        outcome: ExecutionOutcome::BackendError { message: summary },
        duration,
        stdout_tail: None,
        stderr_tail: None,
        artifact_manifest: None,
        outputs,
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

const PREVIEW_LIMIT: usize = 2048;

fn truncate_preview(s: &str) -> String {
    if s.len() <= PREVIEW_LIMIT {
        return s.to_string();
    }
    let mut cut = PREVIEW_LIMIT;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = s[..cut].to_string();
    out.push_str("\n…");
    out
}
