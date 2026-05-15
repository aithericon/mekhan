//! Pre-dispatch hook extension point for effect transitions.
//!
//! This module implements the general-purpose extension point spec'd in
//! `docs/proposals/pre-dispatch-hook.md`. External consumers (cloud-layer
//! capability routing, tenant-quota enforcement, telemetry, chaos injectors,
//! …) register `PreDispatchHook` impls on the `NetRegistry`; the engine
//! invokes the chain immediately before `EffectHandler::execute` and honours
//! `Continue { enriched_effect_config }` / `Reject` / `Defer` outcomes.
//!
//! The hook is consumer-agnostic — no cloud-layer / capability-routing /
//! model-registry vocabulary leaks into this module.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use petri_domain::{PreDispatchOutcomeKind, TransitionId};

// Re-export the domain-side event payload types so consumers of this module
// can pick them up via the application crate without reaching into domain.
pub use petri_domain::{
    PreDispatchHookOutcome, PreDispatchOutcomeKind as DomainPreDispatchOutcomeKind,
};

// ============================================================================
// Trait + context + outcome (spec § 2-4)
// ============================================================================

/// Pre-dispatch hook invoked immediately before `EffectHandler::execute` for
/// every firing effect transition in `ExecutionMode::Live`. Replay mode skips
/// hooks entirely.
///
/// Hooks MUST be `Send + Sync` and object-safe so the registry can store them
/// as `Arc<dyn PreDispatchHook>`.
#[async_trait::async_trait]
pub trait PreDispatchHook: Send + Sync {
    /// Called immediately before `EffectHandler::execute` for a firing
    /// effect transition. The hook MUST NOT perform side effects that
    /// would be re-applied on replay; replay mode skips hooks entirely.
    async fn pre_dispatch(
        &self,
        ctx: &PreDispatchContext<'_>,
    ) -> Result<PreDispatchOutcome, PreDispatchError>;

    /// Human-readable hook name for logging and event records.
    fn name(&self) -> &str;
}

/// Read-mostly view of the dispatch attempt. Hooks DO NOT mutate the
/// context in place; they return modifications via
/// [`PreDispatchOutcome::Continue`].
pub struct PreDispatchContext<'a> {
    /// Net the transition belongs to.
    pub net_id: &'a str,

    /// The transition about to fire.
    pub transition_id: &'a TransitionId,
    pub transition_name: &'a str,
    /// Optional `effect_handler_id` declared on the transition.
    pub effect_handler_id: Option<&'a str>,

    /// Bound input tokens (port name → JSON), as `EffectInput::inputs`.
    pub inputs: &'a HashMap<String, serde_json::Value>,
    /// Read-arc tokens (port name → JSON), as `EffectInput::read_inputs`.
    pub read_inputs: &'a HashMap<String, serde_json::Value>,

    /// Resolved effect config from the transition definition (secrets
    /// already resolved by the firing pipeline).
    pub effect_config: Option<&'a serde_json::Value>,

    /// Net-level parameters from `CreateNetRequest.parameters`.
    pub net_parameters: Option<&'a serde_json::Value>,

    /// Per-firing metadata (correlation_id, tenant_id, process_step,
    /// scenario_id, hook-chain index). Owned strings so hooks may clone
    /// for outbound HTTP without borrowing the engine.
    pub metadata: PreDispatchMetadata,
}

/// Per-firing metadata carried into every hook invocation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PreDispatchMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_step: Option<String>,
    /// Zero-based position of this hook in the registered chain.
    pub hook_chain_index: u32,
}

/// Outcome variants returned from `pre_dispatch`.
///
/// The serde representation uses an internally-tagged discriminator
/// `outcome` with snake_case values — this is the shared wire format
/// for both the Rust trait surface and the HTTP-transport protocol (spec
/// § 7). Adding new variants is a breaking change to both surfaces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PreDispatchOutcome {
    /// Proceed with dispatch. If `enriched_effect_config` is `Some`, the
    /// engine replaces `EffectInput.config` with the enriched value
    /// before calling `EffectHandler::execute`. Enrichment is the only
    /// permitted mutation; inputs/read_inputs are NOT modifiable.
    Continue {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enriched_effect_config: Option<serde_json::Value>,
    },

    /// Abort dispatch. The transition does NOT fire: no tokens consumed,
    /// no tokens produced, no `EffectCompleted`. A `PreDispatchRejected`
    /// event IS emitted. Idempotent w.r.t. marking — retry is the
    /// expected behaviour on next eval pass.
    Reject {
        /// Human-readable reason for the audit log.
        reason: String,
    },

    /// Defer dispatch. Same marking impact as `Reject` (non-destructive),
    /// but the engine schedules a retry after `retry_after`. A
    /// `PreDispatchDeferred` event IS emitted. Counts against a
    /// per-(net_id, transition_id) `max_defers` budget (default 10);
    /// exceeding it escalates to `Reject { reason: "defer-budget-exceeded" }`.
    Defer {
        #[serde(rename = "retry_after_ms", with = "duration_ms")]
        retry_after: Duration,
    },
}

mod duration_ms {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_millis() as u64)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let ms = u64::deserialize(d)?;
        Ok(Duration::from_millis(ms))
    }
}

impl From<&PreDispatchOutcome> for PreDispatchOutcomeKind {
    fn from(o: &PreDispatchOutcome) -> Self {
        match o {
            PreDispatchOutcome::Continue { .. } => Self::Continue,
            PreDispatchOutcome::Reject { .. } => Self::Reject,
            PreDispatchOutcome::Defer { .. } => Self::Defer,
        }
    }
}

// ============================================================================
// Errors (spec § 8)
// ============================================================================

#[derive(Debug, Clone, thiserror::Error)]
pub enum PreDispatchError {
    #[error("Hook '{0}' timed out after {1:?}")]
    Timeout(String, std::time::Duration),

    #[error("Hook '{0}' returned malformed response: {1}")]
    MalformedResponse(String, String),

    #[error("Hook '{0}' transport error: {1}")]
    Transport(String, String),

    #[error("Hook '{0}' panicked")]
    HookPanicked(String),

    #[error("Hook '{0}' execution failed: {1}")]
    ExecutionFailed(String, String),
}

impl PreDispatchError {
    /// Hook name attached to the error variant.
    pub fn hook_name(&self) -> &str {
        match self {
            Self::Timeout(n, _) => n,
            Self::MalformedResponse(n, _) => n,
            Self::Transport(n, _) => n,
            Self::HookPanicked(n) => n,
            Self::ExecutionFailed(n, _) => n,
        }
    }
}

// ============================================================================
// Registration errors (spec § 6)
// ============================================================================

#[derive(Debug, Clone, thiserror::Error)]
pub enum RegistrationError {
    #[error("Cannot register pre-dispatch hook '{0}': registry is frozen (a net is already hot)")]
    RegistryFrozen(String),

    #[error("Pre-dispatch hook '{0}' is already registered")]
    DuplicateName(String),
}

// ============================================================================
// Hook chain (built from TOML config + registered builtins; spec § 5/6)
// ============================================================================

/// A resolved hook in the firing chain, with its TOML-supplied policy
/// settings.
#[derive(Clone)]
pub struct PreDispatchChainEntry {
    pub hook: Arc<dyn PreDispatchHook>,
    pub fail_open: bool,
    pub timeout: Duration,
    /// If non-empty, only fire for transitions whose `effect_handler_id`
    /// is in this list. Empty/absent = fire for every effect transition.
    pub match_effect_handlers: Vec<String>,
}

/// Configuration for a single `[[pre_dispatch_hooks]]` TOML entry (spec § 5).
/// Used by the engine TOML loader and the chain assembler.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreDispatchHookConfig {
    pub name: String,
    pub transport: PreDispatchTransport,
    #[serde(default = "default_fail_open")]
    pub fail_open: bool,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// HTTP-only: target URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Optional: restrict the hook to specific effect_handler_ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub match_effect_handlers: Vec<String>,
    /// Optional: max retries for HTTP-level failures.
    #[serde(default = "default_http_max_retries")]
    pub http_max_retries: u32,
}

fn default_fail_open() -> bool {
    false
}
fn default_timeout_ms() -> u64 {
    500
}
fn default_http_max_retries() -> u32 {
    2
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreDispatchTransport {
    Builtin,
    Http,
}

/// The immutable chain attached to a `NetInstance` once the net goes hot.
/// Built at net-creation time by resolving each `PreDispatchHookConfig`
/// against the registered builtin map + HTTP-transport factory.
#[derive(Clone, Default)]
pub struct PreDispatchChain {
    pub entries: Vec<PreDispatchChainEntry>,
}

impl PreDispatchChain {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

// ============================================================================
// Defer-budget tracker (spec § 11 trip-wire 4 — per-(net_id, transition_id))
// ============================================================================

pub const DEFAULT_MAX_DEFERS: u32 = 10;

/// Per-(net_id, transition_id) defer counter. NOT global — a noisy transition
/// must not starve unrelated transitions' budgets.
#[derive(Default)]
pub struct DeferBudgets {
    counts: Mutex<HashMap<(String, TransitionId), u32>>,
    max_defers: u32,
}

impl DeferBudgets {
    pub fn new(max_defers: u32) -> Self {
        Self {
            counts: Mutex::new(HashMap::new()),
            max_defers,
        }
    }

    pub fn max_defers(&self) -> u32 {
        self.max_defers
    }

    /// Bump the counter and return the new count. Caller compares against
    /// `max_defers()` to detect exhaustion.
    pub fn bump(&self, net_id: &str, transition_id: &TransitionId) -> u32 {
        let mut counts = self.counts.lock();
        let key = (net_id.to_string(), transition_id.clone());
        let entry = counts.entry(key).or_insert(0);
        *entry += 1;
        *entry
    }

    /// Reset the counter on `Continue` so future defers start fresh.
    pub fn reset(&self, net_id: &str, transition_id: &TransitionId) {
        let mut counts = self.counts.lock();
        counts.remove(&(net_id.to_string(), transition_id.clone()));
    }

    /// Current count (read-only, for tests).
    pub fn current(&self, net_id: &str, transition_id: &TransitionId) -> u32 {
        let counts = self.counts.lock();
        counts
            .get(&(net_id.to_string(), transition_id.clone()))
            .copied()
            .unwrap_or(0)
    }
}

// ============================================================================
// Runtime — chain + budgets bundle (threaded through firing.rs)
// ============================================================================

/// Bundle of pre-dispatch state owned by a single `PetriNetService` /
/// `NetInstance`. Threaded through `fire_transition` /
/// `fire_effect_transition` so the engine can evaluate the chain without
/// each call site having to thread two separate args.
#[derive(Clone)]
pub struct PreDispatchRuntime {
    pub chain: Arc<PreDispatchChain>,
    pub budgets: Arc<DeferBudgets>,
    /// Net ID this runtime is bound to — passed into hook contexts.
    pub net_id: String,
}

impl PreDispatchRuntime {
    pub fn new(net_id: impl Into<String>, chain: Arc<PreDispatchChain>) -> Self {
        Self {
            net_id: net_id.into(),
            chain,
            budgets: Arc::new(DeferBudgets::new(DEFAULT_MAX_DEFERS)),
        }
    }

    pub fn empty(net_id: impl Into<String>) -> Self {
        Self::new(net_id, Arc::new(PreDispatchChain::default()))
    }
}

/// Terminal outcome of a single chain evaluation.
#[derive(Clone, Debug)]
pub enum ChainEvalOutcome {
    /// Dispatch may proceed. `enriched_effect_config` is the last hook's
    /// enrichment if any (chain entries can compose enrichments only via
    /// sequential overwrite — the last hook's `Some(...)` wins).
    Continue {
        enriched_effect_config: Option<serde_json::Value>,
    },
    /// Dispatch was rejected by the named hook. Hook chain trace is
    /// returned for the audit event.
    Reject { hook_name: String, reason: String },
    /// Dispatch was deferred by the named hook. Caller is responsible
    /// for bumping the per-(net_id, transition_id) defer counter and
    /// optionally escalating to Reject when the budget is exhausted.
    Defer {
        hook_name: String,
        retry_after: Duration,
    },
}

/// Borrowed view of all per-firing data needed to assemble a
/// `PreDispatchContext` for each hook in the chain. The chain evaluator
/// re-uses the same borrows to construct a fresh context per hook (so the
/// hook-chain-index can be advanced) without cloning the inputs maps.
pub struct ChainEvalInputs<'a> {
    pub net_id: &'a str,
    pub transition_id: &'a TransitionId,
    pub transition_name: &'a str,
    pub effect_handler_id: Option<&'a str>,
    pub inputs: &'a HashMap<String, serde_json::Value>,
    pub read_inputs: &'a HashMap<String, serde_json::Value>,
    pub effect_config: Option<&'a serde_json::Value>,
    pub net_parameters: Option<&'a serde_json::Value>,
    pub metadata_template: PreDispatchMetadata,
}

/// Evaluate the chain. Honours `fail_open`, per-hook `timeout`, and the
/// short-circuit rule: the first hook returning `Reject` or `Defer` wins.
/// Hooks that error are routed through `fail_open`: `true` → treated as
/// Continue (chain advances), `false` → terminal Reject with synthetic
/// reason `"<hook>: hook-error: <message>"`.
///
/// Returns the terminal outcome plus the full per-hook trace for the
/// `PreDispatchEvaluated` event.
pub async fn evaluate_chain(
    chain: &PreDispatchChain,
    inputs: &ChainEvalInputs<'_>,
) -> (ChainEvalOutcome, Vec<PreDispatchHookOutcome>) {
    let mut trace: Vec<PreDispatchHookOutcome> = Vec::with_capacity(chain.entries.len());
    let mut enrichment_so_far: Option<serde_json::Value> = None;

    for (idx, entry) in chain.entries.iter().enumerate() {
        // match_effect_handlers filter
        if !entry.match_effect_handlers.is_empty() {
            match inputs.effect_handler_id {
                Some(id) if entry.match_effect_handlers.iter().any(|m| m == id) => {}
                _ => continue,
            }
        }

        let mut metadata = inputs.metadata_template.clone();
        metadata.hook_chain_index = idx as u32;
        let ctx = PreDispatchContext {
            net_id: inputs.net_id,
            transition_id: inputs.transition_id,
            transition_name: inputs.transition_name,
            effect_handler_id: inputs.effect_handler_id,
            inputs: inputs.inputs,
            read_inputs: inputs.read_inputs,
            effect_config: inputs.effect_config,
            net_parameters: inputs.net_parameters,
            metadata,
        };
        // Apply chain-side enrichment so the next hook sees prior enrichments.
        // We can't mutate the ctx (it's a borrowed view), but we can re-build
        // a tiny override path: the ctx_builder closure already reads the
        // latest enrichment via its closure environment when callers pass one.
        // For now, hooks that compose enrichments observe ONLY the original
        // effect_config; the engine's final dispatched config is the last
        // hook's `Some(...)` enrichment (LWW). Spec § 11 trip-wire 3 commits
        // to chain-in-declaration-order, first short-circuit wins; enrichment
        // composition is unspecified beyond LWW.
        let timeout = entry.timeout;
        let invocation = tokio::time::timeout(timeout, entry.hook.pre_dispatch(&ctx)).await;
        let hook_name = entry.hook.name().to_string();

        match invocation {
            // Timeout fired
            Err(_) => {
                if entry.fail_open {
                    trace.push(PreDispatchHookOutcome {
                        hook_name,
                        kind: PreDispatchOutcomeKind::Continue,
                        reason: None,
                        retry_after_ms: None,
                        fail_open_applied: true,
                    });
                    continue;
                } else {
                    let reason = format!(
                        "{}: hook-error: timeout after {:?}",
                        entry.hook.name(),
                        timeout
                    );
                    trace.push(PreDispatchHookOutcome {
                        hook_name: hook_name.clone(),
                        kind: PreDispatchOutcomeKind::Reject,
                        reason: Some(reason.clone()),
                        retry_after_ms: None,
                        fail_open_applied: false,
                    });
                    return (ChainEvalOutcome::Reject { hook_name, reason }, trace);
                }
            }
            // Hook returned Err
            Ok(Err(err)) => {
                if entry.fail_open {
                    trace.push(PreDispatchHookOutcome {
                        hook_name,
                        kind: PreDispatchOutcomeKind::Continue,
                        reason: None,
                        retry_after_ms: None,
                        fail_open_applied: true,
                    });
                    continue;
                } else {
                    let reason = format!("{}: hook-error: {}", entry.hook.name(), err);
                    trace.push(PreDispatchHookOutcome {
                        hook_name: hook_name.clone(),
                        kind: PreDispatchOutcomeKind::Reject,
                        reason: Some(reason.clone()),
                        retry_after_ms: None,
                        fail_open_applied: false,
                    });
                    return (ChainEvalOutcome::Reject { hook_name, reason }, trace);
                }
            }
            // Hook returned Ok
            Ok(Ok(PreDispatchOutcome::Continue {
                enriched_effect_config,
            })) => {
                if enriched_effect_config.is_some() {
                    enrichment_so_far = enriched_effect_config;
                }
                trace.push(PreDispatchHookOutcome {
                    hook_name,
                    kind: PreDispatchOutcomeKind::Continue,
                    reason: None,
                    retry_after_ms: None,
                    fail_open_applied: false,
                });
                continue;
            }
            Ok(Ok(PreDispatchOutcome::Reject { reason })) => {
                trace.push(PreDispatchHookOutcome {
                    hook_name: hook_name.clone(),
                    kind: PreDispatchOutcomeKind::Reject,
                    reason: Some(reason.clone()),
                    retry_after_ms: None,
                    fail_open_applied: false,
                });
                return (ChainEvalOutcome::Reject { hook_name, reason }, trace);
            }
            Ok(Ok(PreDispatchOutcome::Defer { retry_after })) => {
                trace.push(PreDispatchHookOutcome {
                    hook_name: hook_name.clone(),
                    kind: PreDispatchOutcomeKind::Defer,
                    reason: None,
                    retry_after_ms: Some(retry_after.as_millis() as u64),
                    fail_open_applied: false,
                });
                return (
                    ChainEvalOutcome::Defer {
                        hook_name,
                        retry_after,
                    },
                    trace,
                );
            }
        }
    }

    (
        ChainEvalOutcome::Continue {
            enriched_effect_config: enrichment_so_far,
        },
        trace,
    )
}

// ============================================================================
// HTTP transport (spec § 7)
// ============================================================================

/// HTTP wire-format request body. Mirrors `PreDispatchContext` but
/// owns all data so it can be serialised independently of the engine.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpPreDispatchRequest {
    pub net_id: String,
    pub transition_id: String,
    pub transition_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect_handler_id: Option<String>,
    pub inputs: HashMap<String, serde_json::Value>,
    pub read_inputs: HashMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect_config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net_parameters: Option<serde_json::Value>,
    pub metadata: PreDispatchMetadata,
}

impl<'a> From<&PreDispatchContext<'a>> for HttpPreDispatchRequest {
    fn from(ctx: &PreDispatchContext<'a>) -> Self {
        Self {
            net_id: ctx.net_id.to_string(),
            transition_id: ctx.transition_id.to_string(),
            transition_name: ctx.transition_name.to_string(),
            effect_handler_id: ctx.effect_handler_id.map(|s| s.to_string()),
            inputs: ctx.inputs.clone(),
            read_inputs: ctx.read_inputs.clone(),
            effect_config: ctx.effect_config.cloned(),
            net_parameters: ctx.net_parameters.cloned(),
            metadata: ctx.metadata.clone(),
        }
    }
}

/// HTTP-transport `PreDispatchHook` implementation. The engine ships this
/// builtin so consumers can plug in out-of-process hooks via TOML alone.
pub struct HttpPreDispatchHook {
    name: String,
    url: String,
    timeout: Duration,
    max_retries: u32,
    client: reqwest::Client,
}

impl HttpPreDispatchHook {
    pub fn new(
        name: impl Into<String>,
        url: impl Into<String>,
        timeout: Duration,
        max_retries: u32,
    ) -> Self {
        // A short-lived per-hook client; reqwest's connection pool is
        // happy to be shared, but per-hook isolation makes timeouts easier
        // to reason about.
        let client = reqwest::Client::builder()
            .build()
            .expect("reqwest::Client::builder must not fail with default config");
        Self {
            name: name.into(),
            url: url.into(),
            timeout,
            max_retries,
            client,
        }
    }
}

#[async_trait::async_trait]
impl PreDispatchHook for HttpPreDispatchHook {
    fn name(&self) -> &str {
        &self.name
    }

    async fn pre_dispatch(
        &self,
        ctx: &PreDispatchContext<'_>,
    ) -> Result<PreDispatchOutcome, PreDispatchError> {
        let body: HttpPreDispatchRequest = ctx.into();

        let mut attempt: u32 = 0;
        loop {
            let send_fut = self.client.post(&self.url).json(&body).send();

            let resp_result = tokio::time::timeout(self.timeout, send_fut).await;
            let resp = match resp_result {
                Err(_) => {
                    return Err(PreDispatchError::Timeout(self.name.clone(), self.timeout));
                }
                Ok(Err(e)) => {
                    // Transport-level error — retry with exponential backoff.
                    let err_msg = e.to_string();
                    if attempt >= self.max_retries {
                        return Err(PreDispatchError::Transport(self.name.clone(), err_msg));
                    }
                    let backoff_ms = (50u64).saturating_mul(1u64 << attempt);
                    let backoff =
                        Duration::from_millis(backoff_ms.min(self.timeout.as_millis() as u64));
                    tokio::time::sleep(backoff).await;
                    attempt += 1;
                    continue;
                }
                Ok(Ok(r)) => r,
            };

            let status = resp.status();
            if !status.is_success() {
                // Non-2xx is an application-level error and is NOT retried.
                let body_text = resp.text().await.unwrap_or_default();
                return Err(PreDispatchError::MalformedResponse(
                    self.name.clone(),
                    format!("non-2xx status {}: {}", status.as_u16(), body_text),
                ));
            }

            let body_bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    return Err(PreDispatchError::MalformedResponse(
                        self.name.clone(),
                        format!("response body read error: {}", e),
                    ));
                }
            };

            let outcome: PreDispatchOutcome = match serde_json::from_slice(&body_bytes) {
                Ok(o) => o,
                Err(e) => {
                    return Err(PreDispatchError::MalformedResponse(
                        self.name.clone(),
                        format!("response JSON parse error: {}", e),
                    ));
                }
            };

            return Ok(outcome);
        }
    }
}

// ============================================================================
// Tests — wire-format roundtrip + defer-budget bookkeeping
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_continue_no_enrichment_roundtrips_through_json() {
        let original = PreDispatchOutcome::Continue {
            enriched_effect_config: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(json, r#"{"outcome":"continue"}"#);
        let back: PreDispatchOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn outcome_continue_with_enrichment_roundtrips_through_json() {
        let original = PreDispatchOutcome::Continue {
            enriched_effect_config: Some(serde_json::json!({"queue": "gpu-h100"})),
        };
        let json = serde_json::to_string(&original).unwrap();
        // Matches spec § 7 example 1.
        assert_eq!(
            json,
            r#"{"outcome":"continue","enriched_effect_config":{"queue":"gpu-h100"}}"#
        );
        let back: PreDispatchOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn outcome_reject_roundtrips_through_json() {
        let original = PreDispatchOutcome::Reject {
            reason: "no capability for sae-tier-2".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        // Matches spec § 7 example 2.
        assert_eq!(
            json,
            r#"{"outcome":"reject","reason":"no capability for sae-tier-2"}"#
        );
        let back: PreDispatchOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn outcome_defer_roundtrips_through_json() {
        let original = PreDispatchOutcome::Defer {
            retry_after: Duration::from_millis(1500),
        };
        let json = serde_json::to_string(&original).unwrap();
        // Matches spec § 7 example 3.
        assert_eq!(json, r#"{"outcome":"defer","retry_after_ms":1500}"#);
        let back: PreDispatchOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn defer_budgets_are_keyed_per_net_and_transition() {
        // Per A2 spec § 11 trip-wire #4: budgets are per-(net_id,
        // transition_id), NOT global. A noisy transition must not starve
        // unrelated transitions.
        let budgets = DeferBudgets::new(10);
        let t1 = TransitionId::named("t1");
        let t2 = TransitionId::named("t2");

        // Bump (net_a, t1) twice; (net_a, t2) once; (net_b, t1) once.
        assert_eq!(budgets.bump("net_a", &t1), 1);
        assert_eq!(budgets.bump("net_a", &t1), 2);
        assert_eq!(budgets.bump("net_a", &t2), 1);
        assert_eq!(budgets.bump("net_b", &t1), 1);

        assert_eq!(budgets.current("net_a", &t1), 2);
        assert_eq!(budgets.current("net_a", &t2), 1);
        assert_eq!(budgets.current("net_b", &t1), 1);
        assert_eq!(budgets.current("net_c", &t1), 0);

        // Reset only zeroes the specific key.
        budgets.reset("net_a", &t1);
        assert_eq!(budgets.current("net_a", &t1), 0);
        assert_eq!(budgets.current("net_a", &t2), 1);
        assert_eq!(budgets.current("net_b", &t1), 1);
    }

    #[test]
    fn outcome_kind_projection_is_correct() {
        let cont = PreDispatchOutcome::Continue {
            enriched_effect_config: None,
        };
        let rej = PreDispatchOutcome::Reject { reason: "x".into() };
        let def = PreDispatchOutcome::Defer {
            retry_after: Duration::from_millis(100),
        };
        assert_eq!(
            PreDispatchOutcomeKind::from(&cont),
            PreDispatchOutcomeKind::Continue
        );
        assert_eq!(
            PreDispatchOutcomeKind::from(&rej),
            PreDispatchOutcomeKind::Reject
        );
        assert_eq!(
            PreDispatchOutcomeKind::from(&def),
            PreDispatchOutcomeKind::Defer
        );
    }

    // ========================================================================
    // Cert-plan Tier-2 tests for `evaluate_chain` (spec § 10).
    //
    // Stub hooks let us exercise chain semantics directly without booting the
    // full firing pipeline. Spec test names follow the wording in the cert
    // plan.
    // ========================================================================

    /// Stub `PreDispatchHook` whose outcome is dictated by construction.
    struct StubHook {
        name: String,
        behaviour: StubBehaviour,
        calls: Arc<parking_lot::Mutex<u32>>,
    }

    #[derive(Clone)]
    enum StubBehaviour {
        Continue(Option<serde_json::Value>),
        Reject(String),
        Defer(Duration),
        Error(String),
        Sleep(Duration),
    }

    #[async_trait::async_trait]
    impl PreDispatchHook for StubHook {
        async fn pre_dispatch(
            &self,
            _ctx: &PreDispatchContext<'_>,
        ) -> Result<PreDispatchOutcome, PreDispatchError> {
            *self.calls.lock() += 1;
            match &self.behaviour {
                StubBehaviour::Continue(c) => Ok(PreDispatchOutcome::Continue {
                    enriched_effect_config: c.clone(),
                }),
                StubBehaviour::Reject(r) => Ok(PreDispatchOutcome::Reject { reason: r.clone() }),
                StubBehaviour::Defer(d) => Ok(PreDispatchOutcome::Defer { retry_after: *d }),
                StubBehaviour::Error(msg) => Err(PreDispatchError::ExecutionFailed(
                    self.name.clone(),
                    msg.clone(),
                )),
                StubBehaviour::Sleep(d) => {
                    tokio::time::sleep(*d).await;
                    Ok(PreDispatchOutcome::Continue {
                        enriched_effect_config: None,
                    })
                }
            }
        }

        fn name(&self) -> &str {
            &self.name
        }
    }

    fn stub_hook(
        name: &str,
        behaviour: StubBehaviour,
    ) -> (Arc<StubHook>, Arc<parking_lot::Mutex<u32>>) {
        let calls = Arc::new(parking_lot::Mutex::new(0u32));
        let hook = Arc::new(StubHook {
            name: name.to_string(),
            behaviour,
            calls: calls.clone(),
        });
        (hook, calls)
    }

    fn build_chain(entries: Vec<(Arc<dyn PreDispatchHook>, bool, Duration)>) -> PreDispatchChain {
        PreDispatchChain {
            entries: entries
                .into_iter()
                .map(|(hook, fail_open, timeout)| PreDispatchChainEntry {
                    hook,
                    fail_open,
                    timeout,
                    match_effect_handlers: vec![],
                })
                .collect(),
        }
    }

    fn empty_inputs<'a>(
        transition_id: &'a TransitionId,
        effect_config: Option<&'a serde_json::Value>,
    ) -> (
        HashMap<String, serde_json::Value>,
        HashMap<String, serde_json::Value>,
        ChainEvalInputs<'a>,
    ) {
        let inputs = HashMap::new();
        let read_inputs = HashMap::new();
        let chain_inputs = ChainEvalInputs {
            net_id: "test-net",
            transition_id,
            transition_name: "t",
            effect_handler_id: Some("test_handler"),
            inputs: Box::leak(Box::new(inputs.clone())),
            read_inputs: Box::leak(Box::new(read_inputs.clone())),
            effect_config,
            net_parameters: None,
            metadata_template: PreDispatchMetadata::default(),
        };
        (inputs, read_inputs, chain_inputs)
    }

    #[tokio::test]
    async fn test_continue_passes_through() {
        let (hook, calls) = stub_hook("h1", StubBehaviour::Continue(None));
        let chain = build_chain(vec![(
            hook as Arc<dyn PreDispatchHook>,
            false,
            Duration::from_secs(1),
        )]);
        let tid = TransitionId::named("t1");
        let (_, _, ci) = empty_inputs(&tid, None);
        let (outcome, trace) = evaluate_chain(&chain, &ci).await;
        assert!(matches!(
            outcome,
            ChainEvalOutcome::Continue {
                enriched_effect_config: None
            }
        ));
        assert_eq!(*calls.lock(), 1);
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].kind, PreDispatchOutcomeKind::Continue);
    }

    #[tokio::test]
    async fn test_continue_enriches_config() {
        let enrich = serde_json::json!({"queue": "gpu"});
        let (hook, _) = stub_hook("h1", StubBehaviour::Continue(Some(enrich.clone())));
        let chain = build_chain(vec![(
            hook as Arc<dyn PreDispatchHook>,
            false,
            Duration::from_secs(1),
        )]);
        let tid = TransitionId::named("t1");
        let (_, _, ci) = empty_inputs(&tid, None);
        let (outcome, _) = evaluate_chain(&chain, &ci).await;
        match outcome {
            ChainEvalOutcome::Continue {
                enriched_effect_config: Some(v),
            } => assert_eq!(v, enrich),
            other => panic!("expected Continue+enrich, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_reject_blocks_chain() {
        let (h1, c1) = stub_hook("h1", StubBehaviour::Reject("no".into()));
        let (h2, c2) = stub_hook("h2", StubBehaviour::Continue(None));
        let chain = build_chain(vec![
            (
                h1 as Arc<dyn PreDispatchHook>,
                false,
                Duration::from_secs(1),
            ),
            (
                h2 as Arc<dyn PreDispatchHook>,
                false,
                Duration::from_secs(1),
            ),
        ]);
        let tid = TransitionId::named("t1");
        let (_, _, ci) = empty_inputs(&tid, None);
        let (outcome, trace) = evaluate_chain(&chain, &ci).await;
        match outcome {
            ChainEvalOutcome::Reject { hook_name, reason } => {
                assert_eq!(hook_name, "h1");
                assert_eq!(reason, "no");
            }
            other => panic!("expected Reject, got {:?}", other),
        }
        // Spec § 10 / spec § 11 trip-wire 3: chain short-circuits on first
        // Reject. Honest-absence assertion: hook #2 was NEVER called.
        assert_eq!(*c1.lock(), 1);
        assert_eq!(*c2.lock(), 0);
        // Trace records ONLY the hooks that ran.
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].kind, PreDispatchOutcomeKind::Reject);
    }

    #[tokio::test]
    async fn test_defer_returns_defer_outcome() {
        let (hook, _) = stub_hook("h1", StubBehaviour::Defer(Duration::from_millis(1500)));
        let chain = build_chain(vec![(
            hook as Arc<dyn PreDispatchHook>,
            false,
            Duration::from_secs(1),
        )]);
        let tid = TransitionId::named("t1");
        let (_, _, ci) = empty_inputs(&tid, None);
        let (outcome, trace) = evaluate_chain(&chain, &ci).await;
        match outcome {
            ChainEvalOutcome::Defer {
                hook_name,
                retry_after,
            } => {
                assert_eq!(hook_name, "h1");
                assert_eq!(retry_after, Duration::from_millis(1500));
            }
            other => panic!("expected Defer, got {:?}", other),
        }
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].retry_after_ms, Some(1500));
    }

    #[tokio::test]
    async fn test_chain_short_circuits_on_first_reject() {
        let (h1, c1) = stub_hook("h1", StubBehaviour::Continue(None));
        let (h2, c2) = stub_hook("h2", StubBehaviour::Reject("stop".into()));
        let (h3, c3) = stub_hook("h3", StubBehaviour::Continue(None));
        let chain = build_chain(vec![
            (
                h1 as Arc<dyn PreDispatchHook>,
                false,
                Duration::from_secs(1),
            ),
            (
                h2 as Arc<dyn PreDispatchHook>,
                false,
                Duration::from_secs(1),
            ),
            (
                h3 as Arc<dyn PreDispatchHook>,
                false,
                Duration::from_secs(1),
            ),
        ]);
        let tid = TransitionId::named("t1");
        let (_, _, ci) = empty_inputs(&tid, None);
        let (outcome, _) = evaluate_chain(&chain, &ci).await;
        assert!(matches!(outcome, ChainEvalOutcome::Reject { .. }));
        assert_eq!(*c1.lock(), 1);
        assert_eq!(*c2.lock(), 1);
        // Honest-absence: hook #3 NEVER fires once #2 rejects.
        assert_eq!(*c3.lock(), 0);
    }

    #[tokio::test]
    async fn test_fail_closed_on_hook_error_default() {
        // fail_open = false (default). Error → terminal Reject.
        let (hook, _) = stub_hook("h1", StubBehaviour::Error("boom".into()));
        let chain = build_chain(vec![(
            hook as Arc<dyn PreDispatchHook>,
            false,
            Duration::from_secs(1),
        )]);
        let tid = TransitionId::named("t1");
        let (_, _, ci) = empty_inputs(&tid, None);
        let (outcome, trace) = evaluate_chain(&chain, &ci).await;
        match outcome {
            ChainEvalOutcome::Reject { hook_name, reason } => {
                assert_eq!(hook_name, "h1");
                assert!(
                    reason.contains("hook-error"),
                    "expected hook-error reason, got: {}",
                    reason
                );
            }
            other => panic!("expected Reject under fail-closed, got {:?}", other),
        }
        assert_eq!(trace.len(), 1);
        assert!(!trace[0].fail_open_applied);
    }

    #[tokio::test]
    async fn test_fail_open_on_hook_error_when_configured() {
        let (h1, _) = stub_hook("h1", StubBehaviour::Error("boom".into()));
        let (h2, c2) = stub_hook("h2", StubBehaviour::Continue(None));
        // h1 has fail_open=true → error advances the chain to h2.
        let chain = build_chain(vec![
            (h1 as Arc<dyn PreDispatchHook>, true, Duration::from_secs(1)),
            (
                h2 as Arc<dyn PreDispatchHook>,
                false,
                Duration::from_secs(1),
            ),
        ]);
        let tid = TransitionId::named("t1");
        let (_, _, ci) = empty_inputs(&tid, None);
        let (outcome, trace) = evaluate_chain(&chain, &ci).await;
        assert!(matches!(outcome, ChainEvalOutcome::Continue { .. }));
        assert_eq!(*c2.lock(), 1);
        assert_eq!(trace.len(), 2);
        assert!(trace[0].fail_open_applied);
        assert_eq!(trace[0].kind, PreDispatchOutcomeKind::Continue);
    }

    #[tokio::test]
    async fn test_hook_timeout_fails_closed_by_default() {
        // Hook sleeps longer than the entry's timeout. fail_open=false →
        // terminal Reject with "timeout" reason.
        let (hook, _) = stub_hook("slow", StubBehaviour::Sleep(Duration::from_millis(200)));
        let chain = build_chain(vec![(
            hook as Arc<dyn PreDispatchHook>,
            false,
            Duration::from_millis(20),
        )]);
        let tid = TransitionId::named("t1");
        let (_, _, ci) = empty_inputs(&tid, None);
        let (outcome, _) = evaluate_chain(&chain, &ci).await;
        match outcome {
            ChainEvalOutcome::Reject { reason, .. } => {
                assert!(
                    reason.contains("timeout"),
                    "expected timeout-flagged reject, got: {}",
                    reason
                );
            }
            other => panic!("expected Reject on timeout, got {:?}", other),
        }
    }
}
