//! Rhai guard parsing and identifier extraction for typed-ports scope
//! validation (Phase 3).
//!
//! Guards in Decision/Loop nodes are short Rhai expressions like
//! `approval_step.approved && approval_step.amount > 0`. After Phase 3, each
//! `<ident>.<field>` reference must resolve against the node's upstream scope
//! — `ident` is an upstream node id, `field` a field on that node's output
//! port.
//!
//! This module is intentionally limited to the parser surface of `rhai`:
//! - `parse_guard` syntax-checks via `Engine::compile`.
//! - `extract_qualified_refs` returns the set of `<ident>.<field>` references.
//!
//! Full AST walking via `rhai`'s internal types isn't part of the crate's
//! public API. The regex approach mirrors what
//! `engine/sdk/src/validation.rs::extract_script_variables` already uses and
//! is good enough for identifier-presence checking; we are not doing full
//! type inference over Rhai.

use std::collections::HashSet;

/// A qualified `<node_id>.<field_name>` reference found inside a guard.
///
/// Spans are intentionally omitted — guards are one-liners in practice and
/// the editor doesn't yet have inline annotations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QualifiedRef {
    pub node_id: String,
    pub field: String,
}

/// Syntax-check a Rhai guard. Returns a stable error string usable directly
/// in `CompileError::GuardSyntax`.
///
/// The `[*]` collection-boundary sentinel (`<map_slug>[*].<field>` /
/// `<ht_slug>.<repeater>[*].<field>`) is a borrow-grammar annotation, NOT
/// runtime Rhai — the read-arc synthesis rewrites it into a real `.map(...)`
/// projection over the producer's parked collection AFTER validation. So we
/// strip the sentinel here before the syntax check (`mymap[*].score` →
/// `mymap.score`, which is valid Rhai); the separate `scan_dotted_refs`
/// scanner still sees the `[*]` for ref resolution. `[*]` only ever appears in
/// a borrow ref position, so collapsing it can't mask a genuine syntax error.
pub fn parse_guard(source: &str) -> Result<(), String> {
    let normalized = source.replace("[*]", "");
    let engine = rhai::Engine::new();
    engine
        .compile(&normalized)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Extract all `<ident>.<field>` references from a guard.
///
/// Filters out:
/// - Rhai keywords (`if`, `let`, `true`, `false`, etc.).
/// - Local variables introduced by `let` or `for ... in`.
/// - Chained property access (`a.b.c` yields only `a.b`, not `b.c`).
/// - References inside string literals and `// ...` / `/* ... */` comments.
///
/// The returned set is deduped on `(node_id, field)`.
pub fn extract_qualified_refs(source: &str) -> HashSet<QualifiedRef> {
    let cleaned = strip_comments_and_strings(source);

    let locals = collect_local_vars(&cleaned);

    let mut out = HashSet::new();
    let bytes = cleaned.as_bytes();

    let mut i = 0;
    while i < bytes.len() {
        // Find the start of an identifier — must not be preceded by `.` or
        // by another identifier character (so we only catch the *root* of a
        // property chain).
        if !is_ident_start(bytes[i]) {
            i += 1;
            continue;
        }
        if i > 0 {
            let prev = bytes[i - 1];
            if prev == b'.' || is_ident_cont(prev) {
                i += 1;
                continue;
            }
        }

        // Consume the identifier.
        let start = i;
        while i < bytes.len() && is_ident_cont(bytes[i]) {
            i += 1;
        }
        let ident = &cleaned[start..i];

        // Skip whitespace before the dot — Rhai allows `foo . bar` (rare but
        // legal); be lenient.
        let mut j = i;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'.' {
            continue;
        }
        j += 1;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }

        // The thing after the dot must itself be an identifier (not a number
        // — float-literal patterns like `1.0` don't match because `1` doesn't
        // pass `is_ident_start`).
        if j >= bytes.len() || !is_ident_start(bytes[j]) {
            continue;
        }
        let field_start = j;
        while j < bytes.len() && is_ident_cont(bytes[j]) {
            j += 1;
        }
        let field = &cleaned[field_start..j];

        if RHAI_KEYWORDS.contains(&ident) || locals.contains(ident) {
            continue;
        }

        out.insert(QualifiedRef {
            node_id: ident.to_string(),
            field: field.to_string(),
        });
    }

    out
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Find variables bound by `let x = ...` or `for x in ...` in `source`. They
/// shadow whatever upstream identifier might share the name.
fn collect_local_vars(source: &str) -> HashSet<String> {
    let bytes = source.as_bytes();
    let mut out = HashSet::new();

    // Tiny tokenizer-ish walk — we only care about `let`/`for` followed by an
    // identifier. Match-by-keyword rather than regex to keep dependencies
    // light.
    let mut i = 0;
    while i < bytes.len() {
        let starts_keyword = (bytes[i] == b'l' && source[i..].starts_with("let"))
            || (bytes[i] == b'f' && source[i..].starts_with("for"));
        if !starts_keyword {
            i += 1;
            continue;
        }
        // Boundary check before the keyword.
        if i > 0 && is_ident_cont(bytes[i - 1]) {
            i += 1;
            continue;
        }
        // `let` and `for` are both 3 bytes — no per-keyword branch needed.
        let keyword_len = 3;
        let after = i + keyword_len;
        // Boundary check after the keyword.
        if after >= bytes.len() || is_ident_cont(bytes[after]) {
            i += 1;
            continue;
        }
        // Skip whitespace, then read identifier.
        let mut j = after;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j >= bytes.len() || !is_ident_start(bytes[j]) {
            i += 1;
            continue;
        }
        let s = j;
        while j < bytes.len() && is_ident_cont(bytes[j]) {
            j += 1;
        }
        out.insert(source[s..j].to_string());
        i = j;
    }

    out
}

/// Replace comments and string literals with whitespace so the rest of the
/// scanner can pretend they don't exist. Preserves byte offsets — handy if
/// we ever surface spans.
fn strip_comments_and_strings(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i];
        // Line comment
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
            continue;
        }
        // Block comment
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            out.push(b' ');
            out.push(b' ');
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                i += 1;
            }
            if i + 1 < bytes.len() {
                out.push(b' ');
                out.push(b' ');
                i += 2;
            }
            continue;
        }
        // String literal (double or single quote)
        if c == b'"' || c == b'\'' {
            let quote = c;
            out.push(b' ');
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                    continue;
                }
                out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                i += 1;
            }
            if i < bytes.len() {
                out.push(b' ');
                i += 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }

    // Safety: we only ever emitted ASCII whitespace in place of bytes we
    // removed, so byte-for-byte the result is still valid UTF-8.
    String::from_utf8(out).expect("replaced bytes are ASCII")
}

const RHAI_KEYWORDS: &[&str] = &[
    "true",
    "false",
    "let",
    "const",
    "if",
    "else",
    "switch",
    "for",
    "in",
    "while",
    "loop",
    "do",
    "until",
    "break",
    "continue",
    "return",
    "fn",
    "is_shared",
    "this",
    "import",
    "export",
    "as",
    "global",
    "Fn",
    "call",
    "curry",
    "type_of",
    "print",
    "debug",
    "eval",
    "throw",
    "try",
    "catch",
    "private",
    "public",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_guard() {
        assert!(parse_guard("approval.approved && amount > 0").is_ok());
        assert!(parse_guard("true").is_ok());
    }

    #[test]
    fn parse_invalid_guard() {
        assert!(parse_guard("let x = ;").is_err());
        assert!(parse_guard("if {").is_err());
    }

    #[test]
    fn extract_single_ref() {
        let refs = extract_qualified_refs("approval.approved");
        assert_eq!(refs.len(), 1);
        assert!(refs.contains(&QualifiedRef {
            node_id: "approval".into(),
            field: "approved".into(),
        }));
    }

    #[test]
    fn extract_multiple_refs() {
        let refs = extract_qualified_refs("approval.approved && start.amount > 100");
        assert!(refs.contains(&QualifiedRef {
            node_id: "approval".into(),
            field: "approved".into(),
        }));
        assert!(refs.contains(&QualifiedRef {
            node_id: "start".into(),
            field: "amount".into(),
        }));
    }

    #[test]
    fn skips_chained_property_access() {
        // `start.payload.amount` should only yield `start.payload`, not
        // `payload.amount`.
        let refs = extract_qualified_refs("start.payload.amount > 0");
        assert_eq!(refs.len(), 1);
        assert!(refs.contains(&QualifiedRef {
            node_id: "start".into(),
            field: "payload".into(),
        }));
    }

    #[test]
    fn skips_locals_and_keywords() {
        let refs = extract_qualified_refs("let x = approval.amount; if x > 0 { x.foo }");
        // `x.foo` should be skipped (x is a local). `if` and similar are
        // keywords (not followed by `.`). `approval.amount` is the only ref.
        assert!(refs.contains(&QualifiedRef {
            node_id: "approval".into(),
            field: "amount".into(),
        }));
        assert!(!refs.iter().any(|r| r.node_id == "x"));
    }

    #[test]
    fn skips_for_loop_var() {
        let refs = extract_qualified_refs("for item in items { item.amount }");
        assert!(!refs.iter().any(|r| r.node_id == "item"));
    }

    #[test]
    fn ignores_strings_and_comments() {
        let src = r#"
            // approval.amount in a comment must not count
            /* approval.amount in a block comment must not count */
            let s = "approval.amount in a string must not count";
            real.field == s
        "#;
        let refs = extract_qualified_refs(src);
        assert!(refs.contains(&QualifiedRef {
            node_id: "real".into(),
            field: "field".into(),
        }));
        assert!(!refs.iter().any(|r| r.node_id == "approval"));
    }

    #[test]
    fn ignores_numeric_literals() {
        let refs = extract_qualified_refs("amount > 1.5 && rate < 0.1");
        assert!(refs.is_empty());
    }

    #[test]
    fn whitespace_around_dot() {
        let refs = extract_qualified_refs("approval . approved");
        assert_eq!(refs.len(), 1);
        assert!(refs.contains(&QualifiedRef {
            node_id: "approval".into(),
            field: "approved".into(),
        }));
    }
}
