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
        // A `[*]` collection boundary may appear immediately after the root
        // (`mymap[*].field`) or after any field segment. It is captured as a
        // literal `[*]` sentinel segment so the resolver can distinguish a
        // collection borrow (Map / Repeater Array producer) from a plain
        // scalar borrow. Only a single `[*]` is recognized — nested iteration
        // is unsupported (validated elsewhere). Numeric `[n]` indices are NOT
        // scanned here (guards don't index by position).
        if i + 2 < bytes.len() && bytes[i] == '[' && bytes[i + 1] == '*' && bytes[i + 2] == ']' {
            segs.push("[*]".to_string());
            i += 3;
        }
        while i < bytes.len() && bytes[i] == '.' {
            i += 1;
            let start = i;
            while i < bytes.len() && is_ident(bytes[i]) {
                i += 1;
            }
            if i > start {
                segs.push(bytes[start..i].iter().collect::<String>());
                // A `[*]` boundary may also follow a field segment
                // (`mymap.rows[*].field`). Capture it the same way.
                if i + 2 < bytes.len()
                    && bytes[i] == '['
                    && bytes[i + 1] == '*'
                    && bytes[i + 2] == ']'
                {
                    segs.push("[*]".to_string());
                    i += 3;
                }
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

/// True if `name` occurs in `src` as an identifier **head** — a standalone
/// token (`metals_db`) or the root of a dotted / indexed access
/// (`metals_db.density`, `metals_db[0]`) — but NOT as an attribute tail
/// (`x.metals_db`) and NOT as a substring of a longer identifier
/// (`metals_db_backup`).
///
/// This is the bare-reference complement to [`scan_dotted_refs`], which only
/// yields a head when at least one `.<segment>` follows it. A collection asset
/// is naturally used bare (`len(metals_db)`, `for m in metals_db`), so the
/// dotted scanner misses it; named-global discovery and the borrow source scan
/// step bodies with this against the *known* asset ref-keys, so only a curated
/// library name (never an arbitrary local) is matched. Best-effort text scan —
/// it does not strip strings/comments, so a ref-key named inside a string still
/// matches (harmless: stages an unused asset).
pub(crate) fn references_head_token(src: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let hay: Vec<char> = src.chars().collect();
    let needle: Vec<char> = name.chars().collect();
    let n = needle.len();
    if hay.len() < n {
        return false;
    }
    let mut i = 0;
    while i + n <= hay.len() {
        if hay[i..i + n] == needle[..] {
            let before_ok = i == 0 || (!is_ident(hay[i - 1]) && hay[i - 1] != '.');
            let after = i + n;
            let after_ok = after >= hay.len() || !is_ident(hay[after]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
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

#[cfg(test)]
mod head_token_tests {
    use super::references_head_token;

    #[test]
    fn matches_bare_token() {
        // The case the dotted scanner misses: a collection asset used bare.
        assert!(references_head_token("material_count = len(metals_db)", "metals_db"));
        assert!(references_head_token("for m in metals_db:\n    pass", "metals_db"));
        assert!(references_head_token("metals_db", "metals_db"));
    }

    #[test]
    fn matches_dotted_and_indexed_root() {
        assert!(references_head_token("x = steel_spec.yield_strength", "steel_spec"));
        assert!(references_head_token("first = metals_db[0]", "metals_db"));
    }

    #[test]
    fn rejects_attribute_tail() {
        // `metals_db` as an attribute of something else is NOT a head reference.
        assert!(!references_head_token("x = other.metals_db", "metals_db"));
    }

    #[test]
    fn rejects_longer_identifier() {
        assert!(!references_head_token("x = metals_db_backup", "metals_db"));
        assert!(!references_head_token("x = my_metals_db", "metals_db"));
    }

    #[test]
    fn rejects_absent_and_empty() {
        assert!(!references_head_token("x = len(rows)", "metals_db"));
        assert!(!references_head_token("", "metals_db"));
        assert!(!references_head_token("anything", ""));
    }
}
