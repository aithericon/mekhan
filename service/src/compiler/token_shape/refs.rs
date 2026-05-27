#![allow(unused_imports)]

use std::collections::BTreeMap;

use serde_json::Value;

use crate::compiler::error::CompileError;
use crate::compiler::graph::{topo_order, WorkflowDiGraph};
use crate::models::template::{
    FieldKind, JoinMode, MergeStrategy, Port, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};

use super::*;

// ─── One guard-reference resolver ───────────────────────────────────────────
// (`RefRoot`, `GuardRef`, `guard_refs`, `RefResolution`, `resolve_ref`,
// `reachable_scope`, and `check_guard` moved to
// `crate::compiler::borrow::planners::guard`.)


// ─── Tiny guard expression scanner ──────────────────────────────────────────
//
// `rhai_scope::extract_qualified_refs` only yields 2-segment `ident.field`
// refs; we need full dotted paths *and* the comparison literal for the type
// check. This is a deliberately small scanner — not a Rhai parser.

#[derive(Debug, Clone)]
pub(crate) enum LitTy {
    Number,
    Bool,
    Str,
}

impl LitTy {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            LitTy::Number => "number",
            LitTy::Bool => "bool",
            LitTy::Str => "string",
        }
    }
}

pub(crate) fn scalar_satisfies(ty: &ScalarTy, lit: &LitTy) -> bool {
    matches!(
        (ty, lit),
        (ScalarTy::Number, LitTy::Number)
            | (ScalarTy::Bool, LitTy::Bool)
            | (ScalarTy::String, LitTy::Str)
            | (ScalarTy::Timestamp, LitTy::Str)
            | (ScalarTy::Json, _)
    )
}

/// Scan every contiguous `<root>.<a>.<b>...` dotted reference in `src`, paired
/// with the literal it is compared against on the immediate RHS (best-effort,
/// for the type check). `<root>` is any identifier — `input` (the control
/// token) or a node slug (`<slug>.<field>`, borrowed parked-producer data).
/// This is the single scanner feeding `guard_refs` (and through it
/// `reachable_scope`, `check_guard` and `guard_readarc_plan`) so the picker,
/// the read-arc synthesis and the diagnostics can never disagree.
pub(crate) fn scan_dotted_refs(src: &str) -> Vec<(String, Vec<String>, Option<LitTy>)> {
    let bytes: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        // A root starts an identifier that is not itself the field half of a
        // longer chain (`a.b` must not also yield root `b`).
        let root_start = (bytes[i].is_ascii_alphabetic() || bytes[i] == '_')
            && (i == 0 || (!is_ident(bytes[i - 1]) && bytes[i - 1] != '.'));
        if !root_start {
            i += 1;
            continue;
        }
        let rs = i;
        while i < bytes.len() && is_ident(bytes[i]) {
            i += 1;
        }
        let root: String = bytes[rs..i].iter().collect();
        let mut segs = Vec::new();
        while i < bytes.len() && bytes[i] == '.' {
            i += 1;
            let start = i;
            while i < bytes.len() && is_ident(bytes[i]) {
                i += 1;
            }
            if i > start {
                segs.push(bytes[start..i].iter().collect::<String>());
            } else {
                break;
            }
        }
        if !segs.is_empty() {
            let lit = sniff_rhs_literal(&bytes, i);
            out.push((root, segs, lit));
        }
    }
    out
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Peek past whitespace + a comparison operator and classify the next literal.
fn sniff_rhs_literal(b: &[char], mut i: usize) -> Option<LitTy> {
    while i < b.len() && b[i].is_whitespace() {
        i += 1;
    }
    // skip a comparison operator
    let ops = ['<', '>', '=', '!'];
    if i < b.len() && ops.contains(&b[i]) {
        i += 1;
        if i < b.len() && b[i] == '=' {
            i += 1;
        }
    } else {
        return None;
    }
    while i < b.len() && b[i].is_whitespace() {
        i += 1;
    }
    if i >= b.len() {
        return None;
    }
    if b[i] == '"' || b[i] == '\'' {
        return Some(LitTy::Str);
    }
    let rest: String = b[i..].iter().collect();
    if rest.starts_with("true") || rest.starts_with("false") {
        return Some(LitTy::Bool);
    }
    if b[i].is_ascii_digit() || b[i] == '-' {
        return Some(LitTy::Number);
    }
    None
}
