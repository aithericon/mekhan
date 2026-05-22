//! Rhai source-generation toolkit: literal/interpolation codegen, the
//! null-safe `__pluck` prelude, and the reusable Petri topology builders
//! (retry, merge, join, human-task injection).

use crate::models::template::{
    BackoffKind, MergeStrategy, RetryPolicy, WorkflowNode, WorkflowNodeData,
};
use aithericon_sdk::{
    effects, Context, DynamicToken, EffectError, ExecutorSubmitInput, PlaceHandle, TimerInput,
    TimerSchedule, TimerScheduled,
};
use serde_json::{json, Value};

/// Convert a serde_json::Value to a Rhai literal expression.
pub(crate) fn json_to_rhai_literal(value: &Value) -> String {
    match value {
        Value::Null => "()".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            let escaped = s
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r");
            format!("\"{}\"", escaped)
        }
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_to_rhai_literal).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(obj) => {
            let entries: Vec<String> = obj
                .iter()
                .map(|(k, v)| {
                    let escaped_key = k.replace('\\', "\\\\").replace('"', "\\\"");
                    format!("\"{}\": {}", escaped_key, json_to_rhai_literal(v))
                })
                .collect();
            format!("#{{{}}}", entries.join(", "))
        }
    }
}

/// Escape a string for embedding inside a Rhai double-quoted literal.
/// Mirrors the `Value::String` arm of [`json_to_rhai_literal`] exactly so
/// non-interpolated content stays byte-for-byte identical.
pub(crate) fn rhai_str_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Null-safe path walker prepended to any generated script that contains a
/// `{{ … }}` interpolation. Walks string/int segments; the moment the current
/// value isn't a traversable map/array (or the key/index is absent) it yields
/// `()` instead of raising a Rhai error.
///
/// Rationale: a `{{ a.b }}` placeholder used to compile to the raw accessor
/// `input.a.b`. When `a` is a plain string (e.g. a file field supplied as a
/// bare storage key instead of an upload object), `string.b` is a *hard* Rhai
/// error — unlike a missing map key, which already yields `()`. A hard error
/// in a pure edge transition means its input token is never consumed, so the
/// per-net eval loop retries the transition every cycle forever (observed: one
/// bad `{{ invoice_file.url }}` produced 50k+ `ErrorOccurred` events and wedged
/// net cancellation). Routing every placeholder through `__pluck` makes the
/// whole class degrade to an empty string instead.
pub(crate) const PLUCK_HELPER: &str = "fn __pluck(__r, __segs) { \
for __s in __segs { \
let __t = type_of(__r); \
if __t == \"map\" && type_of(__s) == \"string\" { __r = __r[__s]; continue; } \
if __t == \"array\" && type_of(__s) == \"i64\" && __s >= 0 && __s < __r.len() { __r = __r[__s]; continue; } \
return (); \
} __r } ";

/// Prepend [`PLUCK_HELPER`] to `logic` iff it actually calls `__pluck(`.
///
/// Centralizes the brittle `contains("__pluck(")` check that the Start,
/// PhaseUpdate, ProgressUpdate, Failure and human-task-injection codegen each
/// used to spell out by hand — placeholder-free scripts stay byte-identical
/// (no prelude), interpolated ones get exactly one helper copy.
pub(crate) fn with_pluck_prelude(logic: &str) -> String {
    if logic.contains("__pluck(") {
        format!("{PLUCK_HELPER}{logic}")
    } else {
        logic.to_string()
    }
}

/// Validate a `{{ … }}` placeholder body and turn it into a safe, null-safe
/// Rhai accessor rooted at the workflow token (`input`).
///
/// Only dotted identifier paths with optional numeric indices are accepted —
/// e.g. `invoice_file.url`, `items[0].amount`. This is deliberately *not* a
/// Rhai expression evaluator: arbitrary expressions are rejected (returns
/// `None`) so a template author can never inject executable Rhai through a
/// task block string. The accepted path is emitted as a [`PLUCK_HELPER`]
/// call so a misaimed placeholder degrades to `()` rather than hard-erroring.
pub(crate) fn placeholder_to_accessor(inner: &str) -> Option<String> {
    let s = inner.trim();
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = 0;

    fn ident(bytes: &[u8], i: &mut usize) -> bool {
        let start = *i;
        if *i < bytes.len() && (bytes[*i].is_ascii_alphabetic() || bytes[*i] == b'_') {
            *i += 1;
            while *i < bytes.len() && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'_') {
                *i += 1;
            }
        }
        *i > start
    }

    // Collect path segments as Rhai literals: identifiers (validated to
    // `[A-Za-z0-9_]`, so safe unquoted-escaped) become quoted string keys,
    // `[n]` becomes a bare integer index. Emitted as `__pluck(input, [..])`.
    let mut segs: Vec<String> = Vec::new();
    let first = i;
    if !ident(bytes, &mut i) {
        return None;
    }
    segs.push(format!("\"{}\"", &s[first..i]));

    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                i += 1;
                let seg_start = i;
                if !ident(bytes, &mut i) {
                    return None;
                }
                segs.push(format!("\"{}\"", &s[seg_start..i]));
            }
            b'[' => {
                i += 1;
                let num_start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == num_start || i >= bytes.len() || bytes[i] != b']' {
                    return None;
                }
                segs.push(s[num_start..i].to_string());
                i += 1; // consume ']'
            }
            _ => return None,
        }
    }
    Some(format!("__pluck(input, [{}])", segs.join(", ")))
}

/// Turn a raw string that may contain `{{ path }}` placeholders into a Rhai
/// *expression* (not a quoted literal). Strings with no valid placeholder are
/// emitted exactly as [`json_to_rhai_literal`] would, so existing static
/// content is unchanged. Strings with placeholders become a parenthesised
/// concatenation seeded with `""` to force string context at runtime.
pub(crate) fn interpolate_to_rhai_expr(raw: &str) -> String {
    enum Piece {
        Lit(String),
        Expr(String),
    }

    let mut pieces: Vec<Piece> = Vec::new();
    let mut lit = String::new();
    let mut rest = raw;

    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        if let Some(close_rel) = after.find("}}") {
            let inner = &after[..close_rel];
            if let Some(accessor) = placeholder_to_accessor(inner) {
                lit.push_str(&rest[..open]);
                if !lit.is_empty() {
                    pieces.push(Piece::Lit(std::mem::take(&mut lit)));
                }
                pieces.push(Piece::Expr(accessor));
                rest = &after[close_rel + 2..];
                continue;
            }
            // Not a valid path — keep the literal braces and move past them.
            lit.push_str(&rest[..open + 2]);
            rest = after;
        } else {
            // No closing `}}` — the remainder is all literal.
            break;
        }
    }
    lit.push_str(rest);

    if pieces.is_empty() {
        return format!("\"{}\"", rhai_str_escape(raw));
    }
    if !lit.is_empty() {
        pieces.push(Piece::Lit(lit));
    }

    let mut expr = String::from("(\"\"");
    for p in pieces {
        match p {
            Piece::Lit(s) => {
                expr.push_str(" + \"");
                expr.push_str(&rhai_str_escape(&s));
                expr.push('"');
            }
            Piece::Expr(acc) => {
                expr.push_str(" + (");
                expr.push_str(&acc);
                expr.push(')');
            }
        }
    }
    expr.push(')');
    expr
}

/// Like [`json_to_rhai_literal`] but every string is run through
/// [`interpolate_to_rhai_expr`], so `{{ token.path }}` placeholders anywhere
/// in a human task's steps resolve against the runtime token.
pub(crate) fn json_to_rhai_interpolated(value: &Value) -> String {
    match value {
        Value::String(s) => interpolate_to_rhai_expr(s),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_to_rhai_interpolated).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(obj) => {
            let entries: Vec<String> = obj
                .iter()
                .map(|(k, v)| {
                    let escaped_key = k.replace('\\', "\\\\").replace('"', "\\\"");
                    format!("\"{}\": {}", escaped_key, json_to_rhai_interpolated(v))
                })
                .collect();
            format!("#{{{}}}", entries.join(", "))
        }
        other => json_to_rhai_literal(other),
    }
}

/// Build the retry-then-error topology for an `AutomatedStep`, consuming the
/// executor lifecycle's `failed`/`timed_out` outputs.
///
/// Both failure sources are normalised into a single `failure` place. While
/// `retries < max_retries` the job is re-dispatched by producing a fresh
/// submit token back into the lifecycle `inbox` (which re-fires `submit` — a
/// new executor dispatch, valid for Mekhan's long-lived worker backends).
/// `Immediate` re-dispatches at once; `Fixed`/`Exponential` route through the
/// durable `timer_schedule` effect first (`delay = base` / `base << attempt`).
/// Once retries are exhausted the token is routed to `p_error` (the node's
/// error output), making failures observable / wirable into the graph.
///
/// Called inside the step's `scoped_prefix`, so every id here is namespaced
/// per step and can't collide across automated steps.
pub(crate) fn build_retry_topology(
    ctx: &mut Context,
    policy: &RetryPolicy,
    failed: &PlaceHandle<DynamicToken>,
    timed_out: &PlaceHandle<DynamicToken>,
    exec_inbox: &PlaceHandle<ExecutorSubmitInput>,
    effect_errors: &PlaceHandle<EffectError>,
    p_error: &PlaceHandle<DynamicToken>,
) {
    let failure = ctx.state::<DynamicToken>("failure", "Failure");

    // Surface every executor failure/timeout as an `error` log on the process
    // via the existing `process_log_message` effect handler. `on_failed` /
    // `on_timeout` fan out: `f` drives the retry/exhaust policy as before, and
    // a parallel `log` carries a {level,source,message,detail} entry. The
    // detail nests the executor run detail (exit code, stdout/stderr tails)
    // under `executor` so the operator sees why the step failed — the failing
    // step crashed and can't log this itself. `failure_logged` is a sink (no
    // consumer), matching the lifecycle's other log places.
    let failure_log = ctx.state::<DynamicToken>("failure_log", "Failure Log Input");
    let failure_logged = ctx.state::<DynamicToken>("failure_logged", "Failure Logged");

    // Normalise both lifecycle failure sources into one place. Timeouts carry
    // no `detail`; we only need the resubmit-relevant fields.
    ctx.transition("on_failed", "On Failed")
        .auto_input("e", failed)
        .auto_output("f", &failure)
        .auto_output("log", &failure_log)
        .logic(
            r#"
            let d = e.detail;
            let msg = "Automated step failed";
            if type_of(d) == "map" {
                if type_of(d.outcome) == "map" && d.outcome.keys().contains("exit_code") {
                    msg = msg + " (exit code " + d.outcome.exit_code + ")";
                }
                if type_of(d.stderr_tail) == "string" && d.stderr_tail != "" {
                    msg = msg + ": " + d.stderr_tail;
                }
            }
            #{
                f: #{ job_id: e.job_id, run: e.run, retries: e.retries, max_retries: e.max_retries, spec: e.spec, reason: "failed" },
                log: #{ level: "error", source: "executor", message: msg, detail: #{ execution_id: e.execution_id, run: e.run, retries: e.retries, executor: d } }
            }"#,
        );
    ctx.transition("on_timeout", "On Timeout")
        .auto_input("e", timed_out)
        .auto_output("f", &failure)
        .auto_output("log", &failure_log)
        .logic(
            r#"#{ f: #{ job_id: e.job_id, run: e.run, retries: e.retries, max_retries: e.max_retries, spec: e.spec, reason: "timed_out" }, log: #{ level: "error", source: "executor", message: "Automated step timed out", detail: #{ execution_id: e.execution_id, run: e.run, retries: e.retries } } }"#,
        );

    // Project the failure entry through the process log effect handler. Its
    // EffectCompleted is consumed by the causality projector's existing
    // `process_log_message` branch → hpi_logs (no special-casing there).
    ctx.transition("log_failure", "Log Failure")
        .auto_input("message", &failure_log)
        .auto_output("logged", &failure_logged)
        .builtin_effect(&effects::PROCESS_LOG_MESSAGE);

    // Rhai map for the re-dispatched submit token (bumps run + retries).
    let resubmit = r#"#{ job_id: f.job_id, run: f.run + 1, retries: f.retries + 1, max_retries: f.max_retries, spec: f.spec }"#;

    match policy.backoff {
        BackoffKind::Immediate => {
            ctx.transition("retry", "Retry")
                .auto_input("f", &failure)
                .auto_output("job", exec_inbox)
                .guard_rhai("f.retries < f.max_retries")
                .logic(format!("#{{ job: {resubmit} }}"));
        }
        BackoffKind::Fixed | BackoffKind::Exponential => {
            let timer_in = ctx.state::<TimerInput>("retry_timer", "Retry Timer Input");
            let timer_scheduled =
                ctx.state::<TimerScheduled>("retry_timer_scheduled", "Retry Timer Scheduled");
            let retry_signal = ctx.signal::<DynamicToken>("retry_fire", "Retry Fire");

            let base = policy.base_delay_ms;
            // `base << f.retries` == base * 2^retries (retries is small — the
            // guard bounds it by max_retries).
            let delay_expr = match policy.backoff {
                BackoffKind::Exponential => format!("{base} << f.retries"),
                _ => format!("{base}"),
            };

            ctx.transition("retry_arm", "Retry (arm timer)")
                .auto_input("f", &failure)
                .auto_output("timer", &timer_in)
                .guard_rhai("f.retries < f.max_retries")
                .logic(format!(
                    r#"#{{ timer: #{{ delay_ms: {delay_expr}, target_place_id: "{sig}", payload: {resubmit} }} }}"#,
                    sig = retry_signal.id(),
                ));

            ctx.transition("retry_schedule", "Retry (schedule)")
                .timer_schedule_to(TimerSchedule {
                    timer: &timer_in,
                    scheduled: &timer_scheduled,
                    errors: effect_errors,
                    signal: &retry_signal,
                });

            ctx.transition("retry_reinject", "Retry (re-dispatch)")
                .auto_input("j", &retry_signal)
                .auto_output("job", exec_inbox)
                .logic(r#"#{ job: j }"#);
        }
    }

    // Retries exhausted (or max_retries == 0): surface as the node error.
    ctx.transition("exhausted", "Retries Exhausted")
        .auto_input("f", &failure)
        .auto_output("err", p_error)
        .guard_rhai("f.retries >= f.max_retries")
        .logic(r#"#{ err: f }"#);
}

pub(crate) fn build_merge_logic(state_var: &str, signal_var: &str) -> String {
    format!(
        "let result = {state_var}; \
         let keys = {signal_var}.keys(); \
         for key in keys {{ result[key] = {signal_var}[key]; }} \
         #{{ done: result }}"
    )
}

/// Rhai for a `ParallelJoin` that folds the tokens arriving on `port_names`
/// (`in_0`, `in_1`, …) into a single `output` token.
///
/// One input → straight pass-through. `ShallowLastWins` copies top-level keys
/// left-to-right so the last branch wins on a collision (the historical
/// intent — the old code emitted an unregistered `merge_maps`, so this also
/// fixes a latent runtime bug). `DeepMerge` recursively merges nested object
/// values via a script-local helper.
pub(crate) fn build_join_merge_logic(port_names: &[String], strategy: MergeStrategy) -> String {
    if port_names.len() == 1 {
        return format!("#{{ output: {} }}", port_names[0]);
    }

    let first = &port_names[0];
    let rest = &port_names[1..];

    match strategy {
        MergeStrategy::ShallowLastWins => {
            let mut s = format!("let result = {first}; ");
            for name in rest {
                s.push_str(&format!("for k in {name}.keys() {{ result[k] = {name}[k]; }} "));
            }
            s.push_str("#{ output: result }");
            s
        }
        MergeStrategy::DeepMerge => {
            let mut s = String::from(
                "fn __deep_merge(a, b) { \
                   let out = a; \
                   for k in b.keys() { \
                     if out.keys().contains(k) && type_of(out[k]) == \"map\" && type_of(b[k]) == \"map\" { \
                       out[k] = __deep_merge(out[k], b[k]); \
                     } else { \
                       out[k] = b[k]; \
                     } \
                   } \
                   out \
                 } ",
            );
            s.push_str(&format!("let result = {first}; "));
            for name in rest {
                s.push_str(&format!("result = __deep_merge(result, {name}); "));
            }
            s.push_str("#{ output: result }");
            s
        }
    }
}

pub(crate) fn build_human_task_injection_logic(target_node: &WorkflowNode) -> String {
    if let WorkflowNodeData::HumanTask {
        task_title,
        instructions_mdsvex,
        steps,
        ..
    } = &target_node.data
    {
        let steps_value = serde_json::to_value(steps).unwrap_or_else(|_| json!([]));
        let steps_rhai = json_to_rhai_interpolated(&steps_value);
        let instructions_expr =
            interpolate_to_rhai_expr(instructions_mdsvex.as_deref().unwrap_or(""));
        let title_expr = interpolate_to_rhai_expr(task_title);

        // Only prepend the helper when an interpolation actually emitted a
        // `__pluck(` call, so placeholder-free human tasks stay byte-identical.
        with_pluck_prelude(&format!(
            "let d = input; \
             d.title = {title_expr}; \
             d.instructions_mdsvex = {instructions_expr}; \
             d.steps = {steps_rhai}; \
             #{{ output: d }}"
        ))
    } else {
        "#{ output: input }".to_string()
    }
}
