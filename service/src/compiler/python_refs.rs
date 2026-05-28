//! Python source ref extractor.
//!
//! Scans Python source for `<ident>.<attr>` access patterns, skipping
//! contents of string literals (single/double, triple-quoted, with optional
//! `r`/`b`/`f`/`u` prefixes) and `#` comments. Used by the compiler's
//! borrow planner to lift Python-side `<slug>.<field>` references into
//! Petri read-arcs against the upstream parked place — the user writes
//! `review.invoice_amount` in Python, the compiler detects it, synthesizes
//! the borrow, stages the producer's data as `review.json`, and the runner
//! exposes `review` as a Python global so the access is a plain attribute
//! lookup with no IPC or `token[...]` ceremony in user code.
//!
//! Lexical only — no Python AST. False positives are filtered downstream
//! by matching against the graph's slug index (only known slugs become
//! borrows). The one known limitation: a local variable shadowing a slug
//! name (`for review in items: review.x`) will still trigger a borrow at
//! compile time; at runtime the local wins, so behavior is correct but
//! the per-step staging is wasted.
//!
//! For the higher-level architecture see `token_shape.rs` (docs/10).

/// One `<ident>.<attr>` site detected in Python source. `head` and `attr`
/// are the literal identifier text; the caller filters by whether `head`
/// matches a known graph slug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonRef {
    pub head: String,
    pub attr: String,
}

/// Walk `src` and return every `<ident>.<attr>` access in source order.
///
/// Chains `a.b.c` emit only the outermost pair `(a, b)` — `(b, c)` is
/// suppressed because `b` here is an attribute lookup, not an authored
/// slug. Strings, comments, and contents of any quoted literal are
/// skipped. Same-name duplicates *are* preserved so the caller can count
/// occurrences for diagnostics; dedupe at the call site if you only want
/// the set.
pub fn extract_python_refs(src: &str) -> Vec<PythonRef> {
    let bytes = src.as_bytes();
    let n = bytes.len();
    let mut out = Vec::new();
    let mut i: usize = 0;

    // The most recently scanned identifier and whether it itself came from
    // the right-hand-side of a dotted access (i.e., it is the `b` in `a.b`).
    // Chain attrs must not be re-emitted as heads of further pairs.
    let mut prev_ident: Option<(String, bool)> = None;

    while i < n {
        let b = bytes[i];

        // Whitespace / newline preserves the ident chain state — Python
        // permits `a .b` (rare but legal).
        if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
            continue;
        }

        // `#` comment to end of line. Breaks the chain.
        if b == b'#' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            prev_ident = None;
            continue;
        }

        // String literal (with optional 1-2 char r/b/f/u prefix).
        if let Some(end) = try_string(bytes, i) {
            i = end;
            prev_ident = None;
            continue;
        }

        // Identifier.
        if is_ident_start(b) {
            let start = i;
            while i < n && is_ident_cont(bytes[i]) {
                i += 1;
            }
            let ident = std::str::from_utf8(&bytes[start..i])
                .unwrap_or("")
                .to_string();
            // A bare identifier here is a potential head — not a chain attr,
            // unless the *next* meaningful char is `.IDENT` and the prior
            // state already had a chain attr (we handle that on the `.` arm).
            prev_ident = Some((ident, false));
            continue;
        }

        // Dot — if the preceding ident is non-empty and the next non-space
        // chars are an identifier, that is one `<head>.<attr>` site.
        if b == b'.' {
            let mut j = i + 1;
            while j < n && matches!(bytes[j], b' ' | b'\t') {
                j += 1;
            }
            if j < n && is_ident_start(bytes[j]) {
                let attr_start = j;
                while j < n && is_ident_cont(bytes[j]) {
                    j += 1;
                }
                let attr = std::str::from_utf8(&bytes[attr_start..j])
                    .unwrap_or("")
                    .to_string();
                if let Some((head, head_is_chain_attr)) = prev_ident.take() {
                    if !head_is_chain_attr {
                        out.push(PythonRef {
                            head,
                            attr: attr.clone(),
                        });
                    }
                    // The attr we just consumed could be the head of further
                    // dots (`a.b.c`); flag it so we don't re-emit it.
                    prev_ident = Some((attr, true));
                } else {
                    // `.attr` with no head (`.5`-style numeric float starting
                    // with a dot is rare in Python source but possible). Treat
                    // attr as if it were a fresh ident for further chaining.
                    prev_ident = Some((attr, false));
                }
                i = j;
                continue;
            }
            // Dot not followed by an identifier — number literal, lone `.`,
            // ellipsis. Break the chain and move on.
            prev_ident = None;
            i += 1;
            continue;
        }

        // Any other byte breaks the chain (`(`, `[`, `,`, operators, etc.).
        prev_ident = None;
        i += 1;
    }
    out
}

fn is_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic()
}

fn is_ident_cont(b: u8) -> bool {
    is_ident_start(b) || b.is_ascii_digit()
}

/// If `bytes[i..]` opens a Python string literal, return the byte index
/// just past its closing quote. Otherwise `None`.
///
/// Handles:
///   - Plain `'…'` / `"…"`
///   - Triple-quoted `'''…'''` / `\"\"\"…\"\"\"`
///   - Single-char prefixes (`r`/`b`/`f`/`u`, any case) and two-char
///     combinations (`rb`, `fr`, etc.), but only when the prefix letter(s)
///     are NOT a continuation of a longer identifier (so `rust = "x"`
///     doesn't get treated as an `r`-prefixed string).
///   - Backslash escapes inside non-triple strings.
fn try_string(bytes: &[u8], i: usize) -> Option<usize> {
    let n = bytes.len();
    if i >= n {
        return None;
    }
    // Optional prefix: up to two letters from r/b/f/u (case-insensitive).
    let mut p = i;
    while p < n && p - i < 2 && is_prefix_letter(bytes[p]) {
        p += 1;
    }
    if p >= n || (bytes[p] != b'\'' && bytes[p] != b'"') {
        return None;
    }
    // If we consumed prefix letters, they must NOT be the tail of a longer
    // identifier (`rust = "x"` ⇒ no string match for the `r`).
    if p > i && i > 0 && is_ident_cont(bytes[i - 1]) {
        return None;
    }
    let quote = bytes[p];
    let triple = p + 2 < n && bytes[p + 1] == quote && bytes[p + 2] == quote;
    if triple {
        let mut k = p + 3;
        while k + 2 < n {
            if bytes[k] == quote && bytes[k + 1] == quote && bytes[k + 2] == quote {
                return Some(k + 3);
            }
            if bytes[k] == b'\\' && k + 1 < n {
                k += 2;
                continue;
            }
            k += 1;
        }
        Some(n)
    } else {
        let mut k = p + 1;
        while k < n {
            if bytes[k] == quote {
                return Some(k + 1);
            }
            if bytes[k] == b'\\' && k + 1 < n {
                k += 2;
                continue;
            }
            if bytes[k] == b'\n' {
                // Unterminated single-line string — bail out, don't swallow
                // the rest of the file.
                return Some(k);
            }
            k += 1;
        }
        Some(n)
    }
}

fn is_prefix_letter(b: u8) -> bool {
    matches!(
        b,
        b'r' | b'b' | b'f' | b'u' | b'R' | b'B' | b'F' | b'U'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(src: &str) -> Vec<(String, String)> {
        extract_python_refs(src)
            .into_iter()
            .map(|r| (r.head, r.attr))
            .collect()
    }

    #[test]
    fn simple_attr_access() {
        assert_eq!(
            refs("a = review.invoice_amount"),
            vec![("review".to_string(), "invoice_amount".to_string())]
        );
    }

    #[test]
    fn chain_emits_only_outer() {
        // `review.config.timeout` is `(review, config)` only — `(config, timeout)`
        // would be wrong (the user authored `review`, not `config`).
        assert_eq!(
            refs("x = review.config.timeout"),
            vec![("review".to_string(), "config".to_string())]
        );
    }

    #[test]
    fn separate_pairs_after_break() {
        // `(...)` resets the chain. `a.b c.d` ⇒ both pairs emitted.
        assert_eq!(
            refs("a.b c.d"),
            vec![
                ("a".to_string(), "b".to_string()),
                ("c".to_string(), "d".to_string())
            ]
        );
    }

    #[test]
    fn skip_comment() {
        // The `#` line is a comment — no refs.
        assert_eq!(refs("# review.invoice_amount\n"), Vec::<(String, String)>::new());
    }

    #[test]
    fn skip_single_quoted_string() {
        assert_eq!(
            refs(r#"x = "review.fake"; y = scan.text"#),
            vec![("scan".to_string(), "text".to_string())]
        );
    }

    #[test]
    fn skip_triple_quoted_string() {
        let src = "doc = \"\"\"a.b c.d\n review.x\"\"\"\nfoo = scan.text";
        assert_eq!(refs(src), vec![("scan".to_string(), "text".to_string())]);
    }

    #[test]
    fn f_string_prefix_is_string() {
        // F-strings contain expressions; for v1 we treat the whole literal
        // as opaque (no refs extracted from inside `{}`).
        assert_eq!(refs(r#"x = f"hello {review.x}""#), Vec::<(String, String)>::new());
    }

    #[test]
    fn prefix_letter_not_ident_tail() {
        // `rust = "x"` is an assignment, not an r-prefixed string. The `r`
        // is part of `rust`, so no string is detected starting at index 0.
        // Then `"x"` IS a string. So no refs.
        assert_eq!(refs(r#"rust = "x""#), Vec::<(String, String)>::new());
    }

    #[test]
    fn ignores_call_argument() {
        // `func(a).b` is method on result of `func(a)`. `a` is inside parens
        // (broken chain), `.b` follows `)` (broken chain) ⇒ no pair.
        assert_eq!(refs("z = func(a).b"), Vec::<(String, String)>::new());
    }

    #[test]
    fn nested_brackets_dont_chain() {
        assert_eq!(refs("z = lst[i].field"), Vec::<(String, String)>::new());
    }

    #[test]
    fn multiple_occurrences_preserved() {
        assert_eq!(
            refs("a = review.x\nb = review.y\nc = review.x"),
            vec![
                ("review".to_string(), "x".to_string()),
                ("review".to_string(), "y".to_string()),
                ("review".to_string(), "x".to_string()),
            ]
        );
    }

    #[test]
    fn dunder_attr_handled() {
        // `obj.__class__` — attr is a dunder. Should still emit.
        assert_eq!(
            refs("t = obj.__class__"),
            vec![("obj".to_string(), "__class__".to_string())]
        );
    }
}
