//! Static extraction of equi-join constraints from a transition guard.
//!
//! `find_valid_binding` (see [`crate::binding`]) enumerates the full
//! `tokens_per_place ^ arity` cross-product of input tokens whenever a guard is
//! present, running the Rhai guard on each combination. For a guarded
//! multi-input transition that is the dominant cost (the binding cliff in
//! `docs/engine/scalability.md`), and it is exactly the pool-net
//! worker×job / gather-correlate path the capacity model rides on (every
//! `.correlate()` / `.correlate_on()` join compiles to a guard of the form
//! `a.k == b.k [&& ...]`).
//!
//! This module recovers the **equi-join structure** from the guard *source*
//! conservatively, so the binder can index input tokens by the join key and
//! probe instead of nested-looping. The contract that keeps it correct:
//!
//! - We only extract equalities that are **necessary conditions** of the guard
//!   — i.e. reachable from the root through `&&` only. If the guard has any
//!   top-level `||`, its root is a disjunction and *no* equality is necessary,
//!   so we extract nothing.
//! - We only extract `portA.path == portB.path` between **distinct** ports
//!   (a genuine cross-token correlation). Negation, `!=`, comparisons,
//!   function calls (`satisfies(...)`), and port-vs-constant filters are
//!   skipped.
//! - The binder **still runs the full guard** on every surviving combination.
//!
//! So a constraint is a *pruning hint*, never a replacement for the guard: a
//! missed pattern only costs us speedup; it can never admit a false binding or
//! skip a valid one. We therefore do not need to replicate Rhai's evaluation
//! semantics — only to recognise a syntactic pattern that is a sound necessary
//! condition.

/// A single equi-join constraint recovered from a guard: the values at
/// `port_a`/`path_a` and `port_b`/`path_b` must be equal for the guard to pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JoinConstraint {
    pub port_a: String,
    pub path_a: Vec<String>,
    pub port_b: String,
    pub path_b: Vec<String>,
}

/// Extract the necessary cross-port equi-join constraints from a guard.
///
/// Returns an empty vec when the guard has no usable equi-join structure (no
/// guard correlation, a disjunctive root, only filters / function calls, etc.)
/// — in which case the binder falls back to the full cross-product.
pub(crate) fn extract_join_constraints(guard: &str) -> Vec<JoinConstraint> {
    // A top-level `||` makes the root a disjunction: no conjunct is necessary.
    if !top_level_double(guard, b'|').is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for conjunct in split_top_level(guard, b'&') {
        if let Some(c) = parse_equi_join(conjunct) {
            out.push(c);
        }
    }
    out
}

/// Parse one `&&`-conjunct as a cross-port equality `A.path == B.path`.
fn parse_equi_join(conjunct: &str) -> Option<JoinConstraint> {
    let eq = top_level_eq(conjunct)?;
    let (lhs, rhs) = (&conjunct[..eq], &conjunct[eq + 2..]);

    let (port_a, path_a) = parse_port_path(lhs)?;
    let (port_b, path_b) = parse_port_path(rhs)?;

    // Must be a cross-token correlation: distinct ports, each with a field path.
    if port_a == port_b || path_a.is_empty() || path_b.is_empty() {
        return None;
    }

    Some(JoinConstraint {
        port_a,
        path_a,
        port_b,
        path_b,
    })
}

/// Parse `ident(.ident)+` into `(port, [field, ...])`. Returns `None` for
/// anything that is not a plain port-field access (literals, function calls,
/// indexing, arithmetic, a bare identifier with no field).
fn parse_port_path(expr: &str) -> Option<(String, Vec<String>)> {
    let mut segments = Vec::new();
    for raw in expr.split('.') {
        let seg = raw.trim();
        if !is_ident(seg) {
            return None;
        }
        segments.push(seg.to_string());
    }
    if segments.len() < 2 {
        return None; // need a port plus at least one field
    }
    let port = segments.remove(0);
    Some((port, segments))
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Byte offsets of top-level (depth-0, outside string/char literals)
/// occurrences of a two-character operator made of the repeated byte `c`
/// (`b'&'` → `&&`, `b'|'` → `||`).
fn top_level_double(s: &str, c: u8) -> Vec<usize> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let ch = b[i];
        if let Some(q) = quote {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match ch {
            b'"' | b'\'' | b'`' => quote = Some(ch),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ if ch == c && depth == 0 && i + 1 < b.len() && b[i + 1] == c => {
                out.push(i);
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Split on top-level `&&` (the byte `c` doubled), returning the segments
/// between operators (no empty trimming of the operator itself).
fn split_top_level(s: &str, c: u8) -> Vec<&str> {
    let positions = top_level_double(s, c);
    if positions.is_empty() {
        return vec![s];
    }
    let mut segments = Vec::with_capacity(positions.len() + 1);
    let mut start = 0;
    for &pos in &positions {
        segments.push(&s[start..pos]);
        start = pos + 2;
    }
    segments.push(&s[start..]);
    segments
}

/// First top-level `==` in `s`, excluding `!=`, `<=`, `>=`, `=>`, `===`.
fn top_level_eq(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth: i32 = 0;
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let ch = b[i];
        if let Some(q) = quote {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match ch {
            b'"' | b'\'' | b'`' => quote = Some(ch),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                let prev = if i > 0 { b[i - 1] } else { 0 };
                let is_eqeq = i + 1 < b.len()
                    && b[i + 1] == b'='
                    && prev != b'!'
                    && prev != b'<'
                    && prev != b'>'
                    && prev != b'='
                    && (i + 2 >= b.len() || b[i + 2] != b'=');
                if is_eqeq {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(pa: &str, fa: &[&str], pb: &str, fb: &[&str]) -> JoinConstraint {
        JoinConstraint {
            port_a: pa.to_string(),
            path_a: fa.iter().map(|s| s.to_string()).collect(),
            port_b: pb.to_string(),
            path_b: fb.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ── The canonical .correlate() / bench shapes ──────────────────────

    #[test]
    fn single_field_correlation() {
        assert_eq!(
            extract_join_constraints("req.grant_id == held.grant_id"),
            vec![c("req", &["grant_id"], "held", &["grant_id"])]
        );
    }

    #[test]
    fn multi_field_correlate_on() {
        // .correlate_on("result", "pending", &["job_id", "run"])
        let g = "result.job_id == pending.job_id && result.run == pending.run";
        assert_eq!(
            extract_join_constraints(g),
            vec![
                c("result", &["job_id"], "pending", &["job_id"]),
                c("result", &["run"], "pending", &["run"]),
            ]
        );
    }

    #[test]
    fn bench_chain_guard() {
        // generators::binding emits p0.key == p1.key && p1.key == p2.key
        let g = "p0.key == p1.key && p1.key == p2.key";
        assert_eq!(
            extract_join_constraints(g),
            vec![
                c("p0", &["key"], "p1", &["key"]),
                c("p1", &["key"], "p2", &["key"]),
            ]
        );
    }

    // ── Conservative skips (must extract nothing / partial) ────────────

    #[test]
    fn function_call_guard_skipped() {
        // presence-pool grant: satisfies(...) is not an equi-join
        assert!(extract_join_constraints("satisfies(claim.requirements, unit.caps)").is_empty());
    }

    #[test]
    fn equi_join_plus_function_call() {
        // index on the equality, still let the guard run satisfies()
        let g = "a.group == b.group && satisfies(a.req, b.caps)";
        assert_eq!(
            extract_join_constraints(g),
            vec![c("a", &["group"], "b", &["group"])]
        );
    }

    #[test]
    fn top_level_or_extracts_nothing() {
        // neither equality is necessary under a disjunction
        assert!(extract_join_constraints("a.x == b.x || a.y == b.y").is_empty());
    }

    #[test]
    fn or_inside_parens_is_fine() {
        // root is still a conjunction; a.x==b.x IS necessary
        let g = "a.x == b.x && (c.flag || d.flag)";
        assert_eq!(
            extract_join_constraints(g),
            vec![c("a", &["x"], "b", &["x"])]
        );
    }

    #[test]
    fn inequality_and_comparisons_skipped() {
        assert!(extract_join_constraints("a.x != b.x").is_empty());
        assert!(extract_join_constraints("a.x >= b.x").is_empty());
        assert!(extract_join_constraints("a.x <= b.x").is_empty());
    }

    #[test]
    fn port_vs_constant_skipped() {
        assert!(extract_join_constraints("a.status == \"done\"").is_empty());
        assert!(extract_join_constraints("a.count == 3").is_empty());
        assert!(
            extract_join_constraints("a.x == 3 && a.y == b.y") == vec![c("a", &["y"], "b", &["y"])]
        );
    }

    #[test]
    fn self_comparison_skipped() {
        // same port both sides is a filter, not a cross-token join
        assert!(extract_join_constraints("a.x == a.y").is_empty());
    }

    #[test]
    fn string_literal_with_operators_not_misparsed() {
        // && inside a string must not split; the conjunct is port-vs-constant
        assert!(extract_join_constraints("a.label == \"x && y\"").is_empty());
    }

    #[test]
    fn nested_field_paths() {
        assert_eq!(
            extract_join_constraints("a.meta.id == b.ref.id"),
            vec![c("a", &["meta", "id"], "b", &["ref", "id"])]
        );
    }

    #[test]
    fn whitespace_tolerant() {
        assert_eq!(
            extract_join_constraints("  a.x==b.x  "),
            vec![c("a", &["x"], "b", &["x"])]
        );
    }

    #[test]
    fn bare_identifier_not_a_join() {
        // no field path → not indexable
        assert!(extract_join_constraints("a == b").is_empty());
    }
}
