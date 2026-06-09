//! Rhai source-generation toolkit: literal/interpolation codegen, the
//! null-safe `__pluck` prelude, and the reusable Petri topology builders
//! (retry, merge, join, human-task injection).

use crate::models::template::{
    BackoffKind, MergeStrategy, RetryPolicy, TaskBlockConfig, TaskStepConfig, WorkflowNode,
    WorkflowNodeData,
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

/// Deep-set helper prepended to HumanTask injection logic when a step
/// carries one or more Repeater blocks. Walks `segs` from `idx`, ensuring
/// every intermediate is a map (rewriting non-map sentinels to `#{}`),
/// then writes `value` at the leaf and returns the rewritten root. Pairs
/// with `__pluck`: each Repeater emits
/// `d.payload = __set_path(d.payload, [<head>, ...<pre>], 0, __pluck(input, [...]))`,
/// which the borrow rewrite then retargets at `d_<producer>`.
pub(crate) const SET_PATH_HELPER: &str = "fn __set_path(__m, __segs, __idx, __v) { \
let __cur = if type_of(__m) == \"map\" { __m } else { #{} }; \
if __idx >= __segs.len() { return __v; } \
let __k = __segs[__idx]; \
if __idx == __segs.len() - 1 { \
__cur[__k] = __v; \
} else { \
let __child = if __cur.keys().contains(__k) && type_of(__cur[__k]) == \"map\" { __cur[__k] } else { #{} }; \
__cur[__k] = __set_path(__child, __segs, __idx + 1, __v); \
} \
__cur \
} ";

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

/// One segment of a parsed `{{ … }}` placeholder path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathSegment {
    /// `.<identifier>` — `[A-Za-z_][A-Za-z0-9_]*`.
    Field(String),
    /// `[<n>]` — non-negative integer.
    Index(usize),
    /// `[*]` — the iteration boundary marker introduced by Feature B. In
    /// ref grammars (`<slug>.tasks[*].title`) this says "the consumer
    /// iterates the parked array here; the segments that follow address
    /// each element". `IndexAll` is **deliberately not lowerable** by
    /// `__pluck` — it is a structural annotation for borrow planning, not a
    /// runtime accessor — so `placeholder_to_accessor` rejects any path
    /// containing it.
    IndexAll,
}

/// Validate a `{{ … }}` placeholder body and return its parsed path
/// segments, or `None` for inputs that aren't a dotted identifier path
/// (optionally with numeric indices). This is deliberately *not* a Rhai
/// expression evaluator: arbitrary expressions are rejected so a template
/// author can never inject executable Rhai through a task block string.
///
/// The first segment is always [`PathSegment::Field`] — a leading `[0]`
/// is illegal.
pub(crate) fn parse_placeholder_segments(inner: &str) -> Option<Vec<PathSegment>> {
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

    let mut segs: Vec<PathSegment> = Vec::new();
    let first = i;
    if !ident(bytes, &mut i) {
        return None;
    }
    segs.push(PathSegment::Field(s[first..i].to_string()));

    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                i += 1;
                let seg_start = i;
                if !ident(bytes, &mut i) {
                    return None;
                }
                segs.push(PathSegment::Field(s[seg_start..i].to_string()));
            }
            b'[' => {
                i += 1;
                // Feature B: `[*]` — iteration boundary. Distinct from a
                // numeric `[<n>]` index; structurally annotates the path
                // and is rejected by `placeholder_to_accessor` (text
                // interpolation can't iterate).
                if i < bytes.len() && bytes[i] == b'*' {
                    i += 1;
                    if i >= bytes.len() || bytes[i] != b']' {
                        return None;
                    }
                    segs.push(PathSegment::IndexAll);
                    i += 1; // consume ']'
                    continue;
                }
                let num_start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == num_start || i >= bytes.len() || bytes[i] != b']' {
                    return None;
                }
                let n: usize = s[num_start..i].parse().ok()?;
                segs.push(PathSegment::Index(n));
                i += 1; // consume ']'
            }
            _ => return None,
        }
    }
    Some(segs)
}

/// Validate a `{{ … }}` placeholder body and turn it into a safe, null-safe
/// Rhai accessor rooted at the workflow token (`input`).
///
/// Only dotted identifier paths with optional numeric indices are accepted —
/// e.g. `invoice_file.url`, `items[0].amount`. The accepted path is emitted as
/// a [`PLUCK_HELPER`] call so a misaimed placeholder degrades to `()` rather
/// than hard-erroring. See [`parse_placeholder_segments`] for the parser.
pub(crate) fn placeholder_to_accessor(inner: &str) -> Option<String> {
    let segs = parse_placeholder_segments(inner)?;
    // `[*]` is a structural iteration boundary for ref grammars, not a
    // runtime accessor — text interpolation can't iterate. Reject the
    // whole placeholder so the source string is left literal.
    if segs.iter().any(|s| matches!(s, PathSegment::IndexAll)) {
        return None;
    }
    let rendered: Vec<String> = segs
        .iter()
        .map(|s| match s {
            PathSegment::Field(f) => format!("\"{f}\""),
            PathSegment::Index(n) => n.to_string(),
            PathSegment::IndexAll => unreachable!("rejected above"),
        })
        .collect();
    Some(format!("__pluck(input, [{}])", rendered.join(", ")))
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
/// [`interpolate_to_rhai_expr`], so `{{ <slug>.<field> }}` (or root-level
/// `{{ field }}`) placeholders anywhere in a human task's steps resolve
/// against the runtime token. Slug-qualified placeholders are rewritten
/// post-merge in [`crate::compiler::compile`]'s `(c3)` phase to pluck
/// against the read-arc-bound producer envelope; bare placeholders stay
/// rooted in the slim control token.
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
/// Once retries are exhausted the disposition depends on `error_handled`:
///
/// - `error_handled == true` — the node's `error` handle is wired to a
///   downstream handler (or this is the pooled path, which ALWAYS routes the
///   exhausted token through a held-consuming release transition). The
///   `exhausted` transition routes the failure token to `p_error` (the node's
///   error output), making the failure observable / wirable into the graph.
///   This is the handled `Result::Err`.
/// - `error_handled == false` — the node's `error` handle is NOT wired. A
///   permanent, retries-exhausted failure must crash the whole net rather than
///   strand a token in a dead-end `p_error` place (which quiesces the net at
///   'running' forever). The `exhausted` transition instead `throw`s — a Rhai
///   `throw` maps to a permanent `ScriptError`, which the engine surfaces as
///   `NetFailed`, flipping the instance to 'failed'. This is the unhandled
///   panic unwinding to the top. `p_error` is unused in this branch (the
///   caller does not even create the place).
///
/// Called inside the step's `scoped_prefix`, so every id here is namespaced
/// per step and can't collide across automated steps.
///
/// `node_label` is embedded into the panic message so an operator sees which
/// step crashed the net.
pub(crate) fn build_retry_topology(
    ctx: &mut Context,
    policy: &RetryPolicy,
    failed: &PlaceHandle<DynamicToken>,
    timed_out: &PlaceHandle<DynamicToken>,
    exec_inbox: &PlaceHandle<ExecutorSubmitInput>,
    effect_errors: &PlaceHandle<EffectError>,
    p_error: Option<&PlaceHandle<DynamicToken>>,
    error_handled: bool,
    node_label: &str,
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
                f: #{ job_id: e.job_id, run: e.run, retries: e.retries, max_retries: e.max_retries, spec: e.spec, executor_namespace: e.executor_namespace, feed_chunks: e.feed_chunks, reason: "failed" },
                log: #{ level: "error", source: "executor", message: msg, detail: #{ execution_id: e.execution_id, run: e.run, retries: e.retries, executor: d } }
            }"#,
        );
    ctx.transition("on_timeout", "On Timeout")
        .auto_input("e", timed_out)
        .auto_output("f", &failure)
        .auto_output("log", &failure_log)
        .logic(
            r#"#{ f: #{ job_id: e.job_id, run: e.run, retries: e.retries, max_retries: e.max_retries, spec: e.spec, executor_namespace: e.executor_namespace, feed_chunks: e.feed_chunks, reason: "timed_out" }, log: #{ level: "error", source: "executor", message: "Automated step timed out", detail: #{ execution_id: e.execution_id, run: e.run, retries: e.retries } } }"#,
        );

    // Project the failure entry through the process log effect handler. Its
    // EffectCompleted is consumed by the causality projector's existing
    // `process_log_message` branch → hpi_logs (no special-casing there).
    ctx.transition("log_failure", "Log Failure")
        .auto_input("message", &failure_log)
        .auto_output("logged", &failure_logged)
        .builtin_effect(&effects::PROCESS_LOG_MESSAGE);

    // Rhai map for the re-dispatched submit token (bumps run + retries).
    // CRITICAL: carry `executor_namespace` (+ `feed_chunks`) forward so the
    // retry re-dispatches to the SAME worker-group namespace. Dropping it makes
    // the engine's executor_submit handler fall back to the bare `executor`
    // namespace, which has no consumer in the unified-dispatch model → the
    // retried job orphans and the net hangs forever.
    let resubmit = r#"#{ job_id: f.job_id, run: f.run + 1, retries: f.retries + 1, max_retries: f.max_retries, spec: f.spec, executor_namespace: f.executor_namespace, feed_chunks: f.feed_chunks }"#;

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

    // Retries exhausted (or max_retries == 0). Two dispositions:
    if error_handled {
        // Handled: surface as the node error output (routes to the wired
        // handler — or, on the pooled path, to the held-consuming release
        // transition). Byte-identical to the historical behavior.
        let p_error =
            p_error.expect("error_handled == true requires a p_error sink for the exhausted token");
        ctx.transition("exhausted", "Retries Exhausted")
            .auto_input("f", &failure)
            .auto_output("err", p_error)
            .guard_rhai("f.retries >= f.max_retries")
            .logic(r#"#{ err: f }"#);
    } else {
        // Unhandled: no error handler is wired, so a permanent failure must
        // crash the net (panic → NetFailed → instance 'failed') instead of
        // stranding a token in a dead-end `p_error`. A Rhai `throw` is a
        // permanent ScriptError. The rich exit-code/stderr detail is already
        // projected to hpi_logs via the parallel `log_failure` branch above
        // (which still fires), so observability is preserved before the crash.
        // No output port — `_deadend` suffix exempts it from the
        // every-transition-wired structural check (mirrors Decision's sink).
        let msg = format!("automated step '{node_label}' failed and no error handler is wired");
        ctx.transition(
            "exhausted_deadend",
            "Retries Exhausted (no handler — crash net)",
        )
        .auto_input("f", &failure)
        .guard_rhai("f.retries >= f.max_retries")
        .logic_rhai(format!("throw \"{}\"", rhai_str_escape(&msg)))
        .done();
    }
}

pub(crate) fn build_merge_logic(state_var: &str, signal_var: &str) -> String {
    format!(
        "let result = {state_var}; \
         let keys = {signal_var}.keys(); \
         for key in keys {{ result[key] = {signal_var}[key]; }} \
         #{{ done: result }}"
    )
}

/// Rhai for a `Join { mode: Any }` branch: single-input passthrough whose
/// output token is the inbound payload and whose `data` output mirrors it
/// (so the parked `p_<id>_data` place holds the same payload). Used per
/// transition by `lower_join` in `Any` mode.
pub(crate) fn build_join_passthrough_logic(port_name: &str) -> String {
    format!("#{{ output: {port_name}, data: {port_name} }}")
}

/// Rhai for a `Join { mode: All }` that folds the tokens arriving on
/// `port_names` (`in_0`, `in_1`, …) into a single `output` token plus a
/// mirror `data` token deposited at the parked `p_<id>_data` place. When
/// `also_stage_data` is false the `data` output is omitted.
///
/// One input → straight pass-through. `ShallowLastWins` copies top-level keys
/// left-to-right so the last branch wins on a collision. `DeepMerge`
/// recursively merges nested object values via a script-local helper.
pub(crate) fn build_join_merge_logic_full(
    port_names: &[String],
    strategy: MergeStrategy,
    also_stage_data: bool,
) -> String {
    let tail = if also_stage_data {
        "#{ output: result, data: result }"
    } else {
        "#{ output: result }"
    };

    if port_names.len() == 1 {
        let only = &port_names[0];
        return if also_stage_data {
            format!("let result = {only}; {tail}")
        } else {
            format!("#{{ output: {only} }}")
        };
    }

    let first = &port_names[0];
    let rest = &port_names[1..];

    match strategy {
        MergeStrategy::ShallowLastWins => {
            let mut s = format!("let result = {first}; ");
            for name in rest {
                s.push_str(&format!(
                    "for k in {name}.keys() {{ result[k] = {name}[k]; }} "
                ));
            }
            s.push_str(tail);
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
            s.push_str(tail);
            s
        }
    }
}

pub(crate) fn build_human_task_injection_logic(target_node: &WorkflowNode) -> String {
    if let WorkflowNodeData::HumanTask {
        task_title,
        instructions_mdsvex,
        steps,
        steps_ref,
        ..
    } = &target_node.data
    {
        // Dynamic form: source `d.steps` at runtime from a producer-namespaced
        // `<slug>.<field>` ref via `__pluck(input, [segs])`. The wire-edge
        // rewrite (`apply_human_task_borrows`) retargets `input` → `d_<producer>`
        // once the read-arc is wired — exactly the Repeater path. Falls back to
        // the static literal when no/invalid ref is set, staying byte-identical.
        let steps_rhai = match steps_ref.as_deref().and_then(parse_steps_ref_segments) {
            Some(segs) => {
                let quoted: Vec<String> = segs
                    .iter()
                    .map(|s| format!("\"{}\"", rhai_str_escape(s)))
                    .collect();
                format!("__pluck(input, [{}])", quoted.join(", "))
            }
            None => {
                let steps_value = serde_json::to_value(steps).unwrap_or_else(|_| json!([]));
                json_to_rhai_interpolated(&steps_value)
            }
        };
        let instructions_expr =
            interpolate_to_rhai_expr(instructions_mdsvex.as_deref().unwrap_or(""));
        let title_expr = interpolate_to_rhai_expr(task_title);

        // Feature B: stage every Repeater block's resolved upstream array
        // into `d.payload[<head>][...<pre>]` so the renderer's
        // `getAtPath(taskData, [head, ...pre])` resolves to the array at
        // task-display time. Each emission carries one `__pluck(input,
        // ["<head>", "<pre[0]>", ...])` whose `apply_human_task_borrows`
        // substring-rewrite retargets to `d_<producer>` once the read-arc
        // is wired. Placeholder-free, Repeater-free tasks stay byte-identical.
        let payload_block = build_repeater_payload_block(steps);

        // Only prepend the helper when an interpolation actually emitted a
        // `__pluck(` call, so placeholder-free human tasks stay byte-identical.
        let body = format!(
            "let d = input; \
             d.title = {title_expr}; \
             d.instructions_mdsvex = {instructions_expr}; \
             d.steps = {steps_rhai}; \
             {payload_block}\
             #{{ output: d }}"
        );
        let with_pluck = with_pluck_prelude(&body);
        // SET_PATH_HELPER only appears when a Repeater contributed an
        // injection; keep it gated so non-Repeater tasks stay byte-identical.
        if body.contains("__set_path(") {
            format!("{SET_PATH_HELPER}{with_pluck}")
        } else {
            with_pluck
        }
    } else {
        "#{ output: input }".to_string()
    }
}

/// Parse a HumanTask `steps_ref` into pluck path segments. Returns `None` when
/// the ref is empty, single-segment, or carries a `[*]` wildcard (the dynamic
/// steps borrow is a whole-value pluck, never an iteration boundary), so the
/// injection silently degrades to the static `steps` literal.
fn parse_steps_ref_segments(raw: &str) -> Option<Vec<String>> {
    let t = raw.trim();
    if t.is_empty() || t.contains("[*]") {
        return None;
    }
    let segs: Vec<String> = t.split('.').map(str::to_string).collect();
    if segs.len() < 2 || segs.iter().any(|s| s.is_empty()) {
        return None;
    }
    Some(segs)
}

/// Parse a Repeater `items_ref` into `(head, pre)` — the slug + the
/// pre-`[*]` path segments. Mirrors the frontend's `parseRepeaterRef`
/// (`app/src/lib/components/tasks/task-form-values.svelte.ts`) and the
/// compiler-side validator's `parse_repeater_ref` (validate.rs); returns
/// `None` for malformed inputs so the injection silently degrades — the
/// validator surfaces a typed error before lowering reaches this point.
fn parse_repeater_items_ref(raw: &str) -> Option<(String, Vec<String>)> {
    let trimmed = raw.trim();
    let star = trimmed.find("[*]")?;
    let before = &trimmed[..star];
    let dot = before.find('.')?;
    let head = before[..dot].to_string();
    if head.is_empty() {
        return None;
    }
    let pre_str = &before[dot + 1..];
    if pre_str.is_empty() {
        return None;
    }
    let pre: Vec<String> = pre_str.split('.').map(str::to_string).collect();
    if pre.iter().any(|s| s.is_empty()) {
        return None;
    }
    Some((head, pre))
}

/// Walk every Repeater block in `steps` and emit one `d.payload = __set_path(...)`
/// statement per unique `(head, pre)` `items_ref` path. Empty when there are no
/// Repeater blocks, preserving the existing byte-identical output for legacy
/// HumanTasks.
fn build_repeater_payload_block(steps: &[TaskStepConfig]) -> String {
    let mut seen: std::collections::BTreeSet<(String, Vec<String>)> =
        std::collections::BTreeSet::new();
    let mut emissions: Vec<String> = Vec::new();
    for step in steps {
        for block in &step.blocks {
            let TaskBlockConfig::Repeater { items_ref, .. } = block else {
                continue;
            };
            let Some((head, pre)) = parse_repeater_items_ref(items_ref) else {
                continue;
            };
            if !seen.insert((head.clone(), pre.clone())) {
                continue;
            }
            let mut segs: Vec<String> = Vec::with_capacity(1 + pre.len());
            segs.push(head.clone());
            segs.extend(pre.iter().cloned());
            let segs_literal = segs
                .iter()
                .map(|s| format!("\"{}\"", rhai_str_escape(s)))
                .collect::<Vec<_>>()
                .join(", ");
            let pluck_segs = segs_literal.clone();
            emissions.push(format!(
                "d.payload = __set_path(d.payload, [{segs_literal}], 0, __pluck(input, [{pluck_segs}])); ",
            ));
        }
    }
    if emissions.is_empty() {
        return String::new();
    }
    // Ensure d.payload starts as a map even when the engine projection
    // emitted `null` / `()` (Option<Value>::None at the wire). __set_path
    // also defends against this internally, but a clean preamble keeps
    // the generated Rhai readable when authors inspect compiled AIR.
    let mut out = String::from("if type_of(d.payload) != \"map\" { d.payload = #{}; } ");
    for e in emissions {
        out.push_str(&e);
    }
    out
}

#[cfg(test)]
mod tests {
    //! HumanTask interpolation contract.
    //!
    //! These lock down the path from authored `{{ … }}` placeholders in a
    //! HumanTask's title / instructions / step blocks all the way to the
    //! Rhai script the wire-edge transition runs. They also pin the known
    //! asymmetry vs. the Python AutomatedStep clean-cut model: at the
    //! HumanTask wire-edge, the `input` Rhai variable is the *single
    //! upstream slim token* on the inbound arc — there is no merged
    //! `<slug>` namespace. So `{{start.invoice_id}}` parses to
    //! `__pluck(input, ["start", "invoice_id"])` and resolves to `()` at
    //! runtime unless the upstream literally exposes a `start` map. The
    //! `dotted_slug_path_*` cases below codify that gap.
    use super::*;
    use crate::models::template::{
        CalloutSeverity, Position, TaskBlockConfig, TaskStepConfig, WorkflowNode, WorkflowNodeData,
    };

    fn ht_node(
        task_title: &str,
        instructions_mdsvex: Option<&str>,
        steps: Vec<TaskStepConfig>,
    ) -> WorkflowNode {
        WorkflowNode {
            id: "ht1".to_string(),
            node_type: "human_task".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::HumanTask {
                label: "Task".to_string(),
                description: None,
                task_title: task_title.to_string(),
                instructions_mdsvex: instructions_mdsvex.map(str::to_string),
                steps,
                steps_ref: None,
                capacity: None,
                requirements: None,
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    // ----- placeholder_to_accessor -----

    #[test]
    fn placeholder_simple_ident() {
        assert_eq!(
            placeholder_to_accessor("invoice_id"),
            Some(r#"__pluck(input, ["invoice_id"])"#.to_string())
        );
    }

    #[test]
    fn placeholder_dotted_path_parses_without_namespace_awareness() {
        // The parser happily accepts `start.invoice_id`. Whether `input`
        // actually has a `start` key at runtime is the runtime's problem
        // — and at the HumanTask wire-edge it does NOT (the inbound arc
        // carries the slim token directly, not a `<slug>`-keyed wrapper).
        assert_eq!(
            placeholder_to_accessor("start.invoice_id"),
            Some(r#"__pluck(input, ["start", "invoice_id"])"#.to_string())
        );
    }

    #[test]
    fn placeholder_array_index() {
        assert_eq!(
            placeholder_to_accessor("items[0].amount"),
            Some(r#"__pluck(input, ["items", 0, "amount"])"#.to_string())
        );
    }

    #[test]
    fn placeholder_segments_parses_wildcard_index() {
        // Feature B: `[*]` is the iteration boundary marker — accepted by
        // the parser so ref grammars can carry it through, but rejected by
        // `placeholder_to_accessor` (the text interpolator can't iterate).
        let segs = parse_placeholder_segments("tasks[*].title").expect("parse");
        assert_eq!(
            segs,
            vec![
                PathSegment::Field("tasks".to_string()),
                PathSegment::IndexAll,
                PathSegment::Field("title".to_string()),
            ]
        );
    }

    #[test]
    fn placeholder_to_accessor_rejects_wildcard() {
        // `[*]` is structural; emitting a runtime accessor would silently
        // pick a single element. The whole placeholder is rejected so the
        // literal text survives — same fail-soft contract as a malformed
        // body.
        assert_eq!(placeholder_to_accessor("tasks[*].title"), None);
        assert_eq!(placeholder_to_accessor("tasks[*]"), None);
    }

    #[test]
    fn placeholder_segments_unterminated_wildcard_fails() {
        // `[*` without `]` is malformed; parser returns None per the
        // existing contract for unterminated bracket forms.
        assert_eq!(parse_placeholder_segments("tasks[*"), None);
        assert_eq!(parse_placeholder_segments("tasks[*x]"), None);
    }

    #[test]
    fn placeholder_trims_whitespace() {
        assert_eq!(
            placeholder_to_accessor("   invoice_id   "),
            Some(r#"__pluck(input, ["invoice_id"])"#.to_string())
        );
    }

    #[test]
    fn placeholder_rejects_empty_and_expressions() {
        assert_eq!(placeholder_to_accessor(""), None);
        assert_eq!(placeholder_to_accessor("   "), None);
        // Arbitrary Rhai is rejected so a placeholder can never inject
        // executable code.
        assert_eq!(placeholder_to_accessor("a + b"), None);
        assert_eq!(placeholder_to_accessor("foo()"), None);
        assert_eq!(placeholder_to_accessor("a.b c"), None);
        assert_eq!(placeholder_to_accessor("1abc"), None); // leading digit
        assert_eq!(placeholder_to_accessor("items[]"), None);
        assert_eq!(placeholder_to_accessor("items[a]"), None); // non-numeric idx
    }

    // ----- interpolate_to_rhai_expr -----

    #[test]
    fn interpolate_no_placeholders_emits_plain_literal() {
        // Regression guard: a placeholder-free string must round-trip to
        // the exact same Rhai literal `json_to_rhai_literal` would emit,
        // so byte-identical AIR is preserved for non-templated content.
        assert_eq!(
            interpolate_to_rhai_expr("Hello, world!\nNext line"),
            json_to_rhai_literal(&Value::String("Hello, world!\nNext line".into()))
        );
    }

    #[test]
    fn interpolate_mixed_text_and_one_placeholder() {
        // `""` seed forces string-context concatenation regardless of the
        // plucked value's type.
        assert_eq!(
            interpolate_to_rhai_expr("Hello {{ name }}!"),
            r#"("" + "Hello " + (__pluck(input, ["name"])) + "!")"#
        );
    }

    #[test]
    fn interpolate_invalid_placeholder_left_as_literal_text() {
        // `{{ a + b }}` isn't a legal path → the `{{ }}` survive as plain
        // characters; the surrounding text is untouched and no pluck call
        // is emitted (so the prelude won't be prepended either).
        let out = interpolate_to_rhai_expr("Sum is {{ a + b }} today");
        assert!(!out.contains("__pluck("));
        assert!(out.contains("{{ a + b }}"));
    }

    #[test]
    fn interpolate_unterminated_brace_keeps_raw_text() {
        let out = interpolate_to_rhai_expr("Stray {{ never closed");
        assert!(!out.contains("__pluck("));
        assert!(out.contains("Stray {{ never closed"));
    }

    #[test]
    fn interpolate_two_placeholders_chain_concat() {
        assert_eq!(
            interpolate_to_rhai_expr("{{a}}/{{b}}"),
            r#"("" + (__pluck(input, ["a"])) + "/" + (__pluck(input, ["b"])))"#
        );
    }

    // ----- build_human_task_injection_logic -----

    #[test]
    fn injection_no_placeholders_skips_pluck_helper() {
        // Regression guard for `with_pluck_prelude`: a HumanTask without
        // any `{{ }}` must produce a script that does NOT carry the
        // helper, so existing static HumanTasks stay byte-identical.
        let node = ht_node("Review the invoice", Some("Open the file"), vec![]);
        let script = build_human_task_injection_logic(&node);
        assert!(
            !script.contains("__pluck("),
            "placeholder-free script should not include __pluck helper, got: {script}"
        );
        assert!(script.contains("d.title = \"Review the invoice\""));
    }

    #[test]
    fn injection_with_placeholder_prepends_pluck_once() {
        let node = ht_node("Review {{ vendor_name }}", None, vec![]);
        let script = build_human_task_injection_logic(&node);
        // PLUCK_HELPER appears exactly once (deduped via with_pluck_prelude).
        assert_eq!(
            script.matches("fn __pluck(").count(),
            1,
            "expected exactly one pluck helper definition, got: {script}"
        );
        // And the title field interpolates.
        assert!(
            script.contains(r#"__pluck(input, ["vendor_name"])"#),
            "title placeholder did not lower to a pluck call: {script}"
        );
    }

    #[test]
    fn injection_interpolates_title_instructions_and_step_block_content() {
        let step = TaskStepConfig {
            id: "s1".into(),
            title: "Confirm {{ vendor_name }}".into(),
            description_mdsvex: Some("for invoice {{ invoice_id }}".into()),
            blocks: vec![
                TaskBlockConfig::Mdsvex {
                    content: "Amount: {{ amount }}".into(),
                },
                TaskBlockConfig::Callout {
                    severity: CalloutSeverity::Info,
                    title: Some("Vendor {{ vendor_name }}".into()),
                    content: "Please check {{ vendor_name }}".into(),
                },
            ],
        };
        let node = ht_node(
            "Review {{ vendor_name }}",
            Some("See {{ invoice_id }} for details"),
            vec![step],
        );
        let script = build_human_task_injection_logic(&node);

        // Title + instructions go through `interpolate_to_rhai_expr` and
        // each becomes its own concat expression.
        assert!(script.contains(r#"__pluck(input, ["vendor_name"])"#));
        assert!(script.contains(r#"__pluck(input, ["invoice_id"])"#));
        // Step block contents go through `json_to_rhai_interpolated` —
        // mdsvex blocks and callout title/content interpolate too.
        assert!(script.contains(r#"__pluck(input, ["amount"])"#));
        // Helper is still injected exactly once even though many calls
        // are emitted across title/instructions/step blocks.
        assert_eq!(script.matches("fn __pluck(").count(), 1);
    }

    #[test]
    fn injection_dotted_slug_path_resolves_against_root_input_not_a_namespace() {
        // Codifies the known gap: a HumanTask placeholder of
        // `{{ start.invoice_id }}` is lowered to a pluck against the
        // root `input` — NOT against a `<slug>`-keyed map. At runtime,
        // the wire-edge transition feeding the HumanTask binds `input`
        // to the upstream slim token directly (no merged `<slug>`
        // wrapper), so `__pluck(input, ["start", "invoice_id"])`
        // degrades to `()` unless the upstream literally produced a
        // `start` map field.
        //
        // This is the asymmetry vs. the Python AutomatedStep clean-cut
        // model, where the compiler's borrow planner stages each
        // referenced upstream node's outputs as `<slug>.json` files. If
        // HumanTask interpolation later adopts that same model (read-arc
        // to each referenced producer's `p_<slug>_data` place), this
        // assertion is the one that flips.
        let node = ht_node("Pay {{ start.invoice_id }} now", None, vec![]);
        let script = build_human_task_injection_logic(&node);
        assert!(
            script.contains(r#"__pluck(input, ["start", "invoice_id"])"#),
            "expected dotted path to lower against root `input`, got: {script}"
        );
        // And critically — NOT against a slug-namespaced map. If the
        // model ever changes, this line is what fails first.
        assert!(
            !script.contains(r#"__pluck(input.start"#) && !script.contains(r#"__pluck(scopes,"#),
            "dotted-path lowering should still hit `input` at the root: {script}"
        );
    }

    // ----- Repeater payload staging (Feature B) -----

    #[test]
    fn injection_without_repeater_emits_no_payload_block() {
        // Byte-identity guard: a HumanTask with no Repeater blocks must
        // not carry __set_path or a d.payload preamble. Existing static
        // HumanTasks stay exactly as they were pre-Feature B.
        let node = ht_node("Review the invoice", Some("Open the file"), vec![]);
        let script = build_human_task_injection_logic(&node);
        assert!(
            !script.contains("__set_path("),
            "no Repeater → no __set_path helper, got: {script}"
        );
        assert!(
            !script.contains("d.payload"),
            "no Repeater → no payload preamble, got: {script}"
        );
    }

    #[test]
    fn injection_with_repeater_stages_payload_via_pluck() {
        // A single Repeater whose items_ref points at `extract.tasks[*]`
        // must emit a __set_path call that writes
        // `d.payload.extract.tasks = __pluck(input, ["extract", "tasks"])`.
        // The borrow rewrite (apply_human_task_borrows) later retargets
        // the inner __pluck to `d_extract` once the read-arc is wired.
        let step = TaskStepConfig {
            id: "s1".into(),
            title: "Review".into(),
            description_mdsvex: None,
            blocks: vec![TaskBlockConfig::Repeater {
                items_ref: "extract.tasks[*]".into(),
                item_label_ref: Some("extract.tasks[*].title".into()),
                blocks: vec![],
                output_slug: "review_tasks".into(),
            }],
        };
        let node = ht_node("T", None, vec![step]);
        let script = build_human_task_injection_logic(&node);
        // Helper + preamble + single emission.
        assert_eq!(
            script.matches("fn __set_path(").count(),
            1,
            "expected exactly one __set_path helper: {script}"
        );
        assert!(
            script.contains("if type_of(d.payload) != \"map\""),
            "missing d.payload preamble: {script}"
        );
        assert!(
            script.contains(
                r#"d.payload = __set_path(d.payload, ["extract", "tasks"], 0, __pluck(input, ["extract", "tasks"]))"#
            ),
            "missing payload-staging emission: {script}"
        );
        // Pluck helper also present (this script contains __pluck).
        assert_eq!(
            script.matches("fn __pluck(").count(),
            1,
            "expected one __pluck helper, got: {script}"
        );
    }

    #[test]
    fn injection_repeaters_with_shared_head_dedupe_paths() {
        // Two Repeaters pointing at the same items_ref produce a single
        // payload-staging emission. Distinct items_refs produce distinct
        // emissions even when the head is shared.
        let step = TaskStepConfig {
            id: "s1".into(),
            title: "Review".into(),
            description_mdsvex: None,
            blocks: vec![
                TaskBlockConfig::Repeater {
                    items_ref: "extract.tasks[*]".into(),
                    item_label_ref: None,
                    blocks: vec![],
                    output_slug: "review_a".into(),
                },
                // Same items_ref — dedupes.
                TaskBlockConfig::Repeater {
                    items_ref: "extract.tasks[*]".into(),
                    item_label_ref: None,
                    blocks: vec![],
                    output_slug: "review_b".into(),
                },
                // Distinct items_ref — separate emission.
                TaskBlockConfig::Repeater {
                    items_ref: "extract.subtasks[*]".into(),
                    item_label_ref: None,
                    blocks: vec![],
                    output_slug: "review_c".into(),
                },
            ],
        };
        let node = ht_node("T", None, vec![step]);
        let script = build_human_task_injection_logic(&node);
        assert_eq!(
            script
                .matches(r#"d.payload = __set_path(d.payload, ["extract", "tasks"]"#)
                .count(),
            1,
            "same items_ref must produce exactly one staging call: {script}"
        );
        assert_eq!(
            script
                .matches(r#"d.payload = __set_path(d.payload, ["extract", "subtasks"]"#)
                .count(),
            1,
            "distinct items_ref must produce its own staging call: {script}"
        );
    }

    #[test]
    fn injection_repeater_with_deep_pre_path_emits_full_segments() {
        // items_ref = "foo.bar.baz[*]" → head=foo, pre=["bar","baz"].
        // The emitted segments must include every pre segment so
        // __set_path drills the full path before writing the leaf.
        let step = TaskStepConfig {
            id: "s1".into(),
            title: "Review".into(),
            description_mdsvex: None,
            blocks: vec![TaskBlockConfig::Repeater {
                items_ref: "foo.bar.baz[*]".into(),
                item_label_ref: None,
                blocks: vec![],
                output_slug: "deep".into(),
            }],
        };
        let node = ht_node("T", None, vec![step]);
        let script = build_human_task_injection_logic(&node);
        assert!(
            script.contains(
                r#"d.payload = __set_path(d.payload, ["foo", "bar", "baz"], 0, __pluck(input, ["foo", "bar", "baz"]))"#
            ),
            "deep pre-path not emitted in full: {script}"
        );
    }

    #[test]
    fn injection_repeater_emits_pluck_borrow_needle_for_rewrite() {
        // The whole point of staging via __pluck(input, ["<slug>", "<attr>"…])
        // is that `apply_human_task_borrows` rewrites that prefix to
        // `__pluck(d_<producer>, [<hoist>, "<attr>"…])` after wiring a
        // read-arc on the upstream parked data. This guards the
        // contract: the emitted needle must match the rewrite pattern
        // (slug + trailing `", `).
        let step = TaskStepConfig {
            id: "s1".into(),
            title: "Review".into(),
            description_mdsvex: None,
            blocks: vec![TaskBlockConfig::Repeater {
                items_ref: "extract.tasks[*]".into(),
                item_label_ref: None,
                blocks: vec![],
                output_slug: "review_tasks".into(),
            }],
        };
        let node = ht_node("T", None, vec![step]);
        let script = build_human_task_injection_logic(&node);
        assert!(
            script.contains(r#"__pluck(input, ["extract", "tasks"]"#),
            "rewrite needle `__pluck(input, [\"extract\", ` missing: {script}"
        );
    }

    #[test]
    fn injection_non_human_task_yields_pass_through() {
        // Defensive: wiring_logic only invokes us for HumanTask, but the
        // function still has a fallback branch — keep it documented.
        let node = WorkflowNode {
            id: "x".into(),
            node_type: "start".into(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Start {
                label: "Start".into(),
                description: None,
                initial: crate::models::template::default_initial_port(),
                process_name: None,
            },
            parent_id: None,
            width: None,
            height: None,
        };
        assert_eq!(
            build_human_task_injection_logic(&node),
            "#{ output: input }"
        );
    }
}
