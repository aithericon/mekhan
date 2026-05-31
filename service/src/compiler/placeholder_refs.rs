//! Shared `{{ <head>.<attr> … }}` placeholder scanner.
//!
//! Every authoring surface that supports `{{ … }}` interpolation — HumanTask
//! markdown/titles, LLM `prompt`/`system_prompt`/`history`/`images.path`,
//! Kreuzberg `file`/`files[]` — runs through this scanner so the lexical
//! contract is byte-identical across backends. The parser in
//! [`parse_placeholder_segments`] (a dotted-path validator, NOT a Rhai
//! evaluator) gates what a placeholder is allowed to be.
//!
//! A scanned [`PlaceholderRef`] is a *candidate* borrow: the caller filters
//! by whether `head` matches a known graph slug. Bare-segment placeholders
//! (`{{ invoice_id }}`) are slim-token references handled elsewhere; only
//! `head.attr` pairs surface from this scanner.
//!
//! [`parse_placeholder_segments`]: super::rhai_gen::parse_placeholder_segments

use crate::compiler::rhai_gen::{parse_placeholder_segments, PathSegment};

/// One `{{ <head>.<attr> … }}` placeholder site detected on a free-form
/// string. `head` is the first path segment (a candidate slug); `attr` is
/// the second (the first field off the slug-namespaced producer envelope).
/// Deeper segments (e.g. `review.config.timeout`) are attribute lookups on
/// the staged envelope and are not surfaced here — only the outermost pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceholderRef {
    pub head: String,
    pub attr: String,
}

/// Scan one free-form string for `{{ … }}` placeholders, return every
/// `(head, attr)` pair where the placeholder validates as a dotted-path
/// accessor with at least two segments.
///
/// - Same-name duplicates are preserved so callers can count occurrences;
///   borrow planners dedupe per `(consumer, producer)`.
/// - Single-segment placeholders (`{{ invoice_id }}`) are skipped — they
///   are slim control-token references, not slug-namespaced borrows.
/// - Placeholders whose second segment is an index (`{{ start[0].x }}`)
///   are also skipped: an index immediately off the head isn't a field
///   access on a slug-namespaced envelope.
/// - Placeholders that fail [`parse_placeholder_segments`] are silently
///   skipped — the runtime keeps them as literal text. Callers that need
///   to reject malformed bodies should validate separately via the parser.
pub fn scan_placeholders(raw: &str) -> Vec<PlaceholderRef> {
    let mut out = Vec::new();
    let mut rest = raw;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close_rel) = after.find("}}") else {
            return out;
        };
        let inner = &after[..close_rel];
        if let Some(segs) = parse_placeholder_segments(inner) {
            if let (Some(PathSegment::Field(head)), Some(second)) = (segs.first(), segs.get(1)) {
                let attr = match second {
                    PathSegment::Field(a) => a.clone(),
                    // Numeric `[N]` or wildcard `[*]` as the second segment
                    // is not a slug-namespaced field access. The wildcard
                    // case (Feature B) is handled by ref grammars, not by
                    // text interpolation — skip and let the literal text
                    // survive to runtime.
                    PathSegment::Index(_) | PathSegment::IndexAll => {
                        rest = &after[close_rel + 2..];
                        continue;
                    }
                };
                out.push(PlaceholderRef {
                    head: head.clone(),
                    attr,
                });
            }
        }
        rest = &after[close_rel + 2..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(s: &str) -> Vec<(String, String)> {
        scan_placeholders(s)
            .into_iter()
            .map(|r| (r.head, r.attr))
            .collect()
    }

    #[test]
    fn extracts_dotted_pairs() {
        assert_eq!(
            pairs("Review {{ start.invoice_id }} for {{ start.vendor_name }}"),
            vec![
                ("start".into(), "invoice_id".into()),
                ("start".into(), "vendor_name".into()),
            ]
        );
    }

    #[test]
    fn skips_single_segment_placeholders() {
        assert!(pairs("Pay {{ invoice_id }}").is_empty());
    }

    #[test]
    fn skips_invalid_placeholders() {
        // `{{ a + b }}` fails parse_placeholder_segments — runtime keeps it
        // as literal text. Unterminated `{{ nope` likewise ignored.
        assert!(pairs("Sum {{ a + b }} and unterminated {{ nope").is_empty());
    }

    #[test]
    fn skips_index_as_second_segment() {
        // `{{ start[0].x }}` — first non-head segment is an index, not a
        // field. Skip per the contract; the runtime degrades to `()`.
        assert!(pairs("{{ start[0].x }}").is_empty());
    }

    #[test]
    fn skips_wildcard_as_second_segment() {
        // Feature B: `{{ tasks[*].title }}` parses but is not a slug-
        // namespaced field access — `[*]` is a ref-grammar construct, not
        // a text interpolation. Skip; the literal text survives.
        assert!(pairs("{{ tasks[*].title }}").is_empty());
    }

    #[test]
    fn chain_emits_only_first_two_segments() {
        // `{{ review.config.timeout }}` — head=review, attr=config; deeper
        // segments are attribute lookups on the staged producer envelope.
        assert_eq!(
            pairs("{{ review.config.timeout }}"),
            vec![("review".into(), "config".into())]
        );
    }

    #[test]
    fn duplicates_preserved() {
        assert_eq!(
            pairs("{{ a.x }} and {{ b.y }} again {{ a.x }}"),
            vec![
                ("a".into(), "x".into()),
                ("b".into(), "y".into()),
                ("a".into(), "x".into()),
            ]
        );
    }
}
