//! HumanTask placeholder ref extractor — node walker that hands each
//! free-form string to the shared [`scan_placeholders`] scanner.
//!
//! Walks every author-visible string on a `HumanTask` node (title,
//! instructions, every step's title/description, every block's
//! markdown / callout / image-pdf caption / url) and returns each
//! `{{ <head>.<attr> … }}` site. The caller filters by whether `head`
//! matches a known graph slug — exactly the same downstream resolution
//! the Python borrow planner uses (`automated_step_borrow_plan`).
//!
//! [`scan_placeholders`]: super::placeholder_refs::scan_placeholders

use crate::compiler::placeholder_refs::{scan_placeholders, PlaceholderRef};
use crate::models::template::{TaskBlockConfig, TaskStepConfig, WorkflowNode, WorkflowNodeData};

/// Re-export of the shared placeholder site type so historical callers can
/// keep the `HumanTaskRef` name they were importing.
pub type HumanTaskRef = PlaceholderRef;

/// Walk a HumanTask `node` and return every `{{ head.attr … }}` site in
/// authoring order. Same-name duplicates are preserved so callers can
/// count occurrences; the borrow planner dedupes per
/// `(consumer, producer)`.
///
/// Returns an empty vec for non-HumanTask nodes (callers can pass any
/// node without a type check).
pub fn extract_human_task_refs(node: &WorkflowNode) -> Vec<HumanTaskRef> {
    let WorkflowNodeData::HumanTask {
        task_title,
        instructions_mdsvex,
        steps,
        steps_ref,
        ..
    } = &node.data
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    scan_into(task_title, &mut out);
    if let Some(s) = instructions_mdsvex {
        scan_into(s, &mut out);
    }
    for step in steps {
        scan_step(step, &mut out);
    }
    // Opt-in dynamic form: the whole `steps` list is borrowed from a producer's
    // `<slug>.<field>` output. Surface that producer ref so the borrow planner
    // synthesizes a read-arc + the wire-edge rewrite retargets `__pluck(input,…)`.
    // We reuse the placeholder scanner by synthesizing a `{{ <ref> }}` site, so
    // the emitted `HumanTaskRef` is byte-identical in shape to a real placeholder
    // ref (no dependence on the struct's internal field layout).
    if let Some(sr) = steps_ref {
        if is_well_formed_steps_ref(sr) {
            scan_into(&format!("{{{{ {} }}}}", sr.trim()), &mut out);
        }
    }
    out
}

/// A `steps_ref` is borrow-eligible when it is a plain `<head>.<attr>[.<more>…]`
/// dotted path (at least two non-empty segments, no `[*]` wildcard). Malformed
/// refs are ignored so the dynamic form silently degrades to its static `steps`.
fn is_well_formed_steps_ref(raw: &str) -> bool {
    let t = raw.trim();
    if t.is_empty() || t.contains("[*]") {
        return false;
    }
    let segs: Vec<&str> = t.split('.').collect();
    segs.len() >= 2 && !segs.iter().any(|s| s.is_empty())
}

fn scan_step(step: &TaskStepConfig, out: &mut Vec<HumanTaskRef>) {
    scan_into(&step.title, out);
    if let Some(s) = &step.description_mdsvex {
        scan_into(s, out);
    }
    for block in &step.blocks {
        scan_block(block, out);
    }
}

fn scan_block(block: &TaskBlockConfig, out: &mut Vec<HumanTaskRef>) {
    match block {
        // Form fields, dividers, files have no free-form strings the
        // runtime interpolates — `Input.field` is a typed schema, not
        // markdown.
        TaskBlockConfig::Input { .. } | TaskBlockConfig::Divider | TaskBlockConfig::File { .. } => {
        }
        TaskBlockConfig::Mdsvex { content } => scan_into(content, out),
        TaskBlockConfig::Callout { title, content, .. } => {
            if let Some(t) = title {
                scan_into(t, out);
            }
            scan_into(content, out);
        }
        TaskBlockConfig::Image { url, caption, .. } => {
            if let Some(u) = url {
                scan_into(u, out);
            }
            if let Some(c) = caption {
                scan_into(c, out);
            }
        }
        TaskBlockConfig::Pdf { url, caption, .. } => {
            if let Some(u) = url {
                scan_into(u, out);
            }
            if let Some(c) = caption {
                scan_into(c, out);
            }
        }
        TaskBlockConfig::Download { downloads } => {
            for item in downloads {
                scan_into(&item.url, out);
                scan_into(&item.filename, out);
                if let Some(d) = &item.description {
                    scan_into(d, out);
                }
            }
        }
        // Feature B Repeater: `items_ref` and `item_label_ref` are
        // structured `<slug>.<field>[*]…` refs (no `{{ … }}` braces).
        // The borrow planner needs the slug + first-field pair so it
        // can synthesize a read-arc on the upstream parked array;
        // `scan_placeholders` skips wildcards (text interpolation
        // contract), so we extract the (head, attr) pair directly.
        //
        // Inner `blocks` may carry their own `{{ … }}` placeholders
        // (e.g. an Mdsvex block authored as
        // `"Review: {{ extract.tasks[*].title }}"`). The compiler's
        // borrow planner still needs the outer slug; per-row resolution
        // happens consumer-side. Recurse so the planner sees every
        // referenced producer.
        TaskBlockConfig::Repeater {
            items_ref,
            item_label_ref,
            blocks,
            ..
        } => {
            if let Some(p) = parse_repeater_ref_head_attr(items_ref) {
                out.push(p);
            }
            if let Some(label_ref) = item_label_ref {
                if let Some(p) = parse_repeater_ref_head_attr(label_ref) {
                    out.push(p);
                }
            }
            for inner in blocks {
                scan_block(inner, out);
            }
        }
    }
}

fn scan_into(raw: &str, out: &mut Vec<HumanTaskRef>) {
    out.extend(scan_placeholders(raw));
}

/// Parse a Repeater `items_ref` / `item_label_ref` as a structured ref
/// (no `{{ … }}` braces) and extract `(head, attr)` — the slug + first
/// field pair the borrow planner uses. Examples:
///
/// - `"extract.tasks[*]"`         → `Some(("extract", "tasks"))`
/// - `"extract.tasks[*].title"`   → `Some(("extract", "tasks"))`
/// - `"foo"`                      → `None` (bare, no field)
/// - `""`                         → `None`
///
/// The compiler validates the rest (wildcard placement, upstream
/// producer existence, array shape) via the standard
/// `resolve_ref`/`scan_dotted_refs` path; this extractor only surfaces
/// the head/attr pair so the read-arc planner can include the producer.
fn parse_repeater_ref_head_attr(raw: &str) -> Option<HumanTaskRef> {
    let trimmed = raw.trim();
    let dot = trimmed.find('.')?;
    let head = &trimmed[..dot];
    if head.is_empty() {
        return None;
    }
    // Attr ends at the first `.` (next segment) or `[` (wildcard / index).
    let after = &trimmed[dot + 1..];
    let attr_end = after.find(['.', '[']).unwrap_or(after.len());
    let attr = &after[..attr_end];
    if attr.is_empty() {
        return None;
    }
    Some(HumanTaskRef {
        head: head.to_string(),
        attr: attr.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::{
        CalloutSeverity, Position, TaskBlockConfig, TaskFieldConfig, TaskFieldKind, TaskStepConfig,
        WorkflowNode, WorkflowNodeData,
    };

    fn ht(
        task_title: &str,
        instructions_mdsvex: Option<&str>,
        steps: Vec<TaskStepConfig>,
    ) -> WorkflowNode {
        WorkflowNode {
            id: "ht1".into(),
            node_type: "human_task".into(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::HumanTask {
                label: "Task".into(),
                description: None,
                task_title: task_title.into(),
                instructions_mdsvex: instructions_mdsvex.map(str::to_string),
                steps,
                steps_ref: None,
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn pairs(node: &WorkflowNode) -> Vec<(String, String)> {
        extract_human_task_refs(node)
            .into_iter()
            .map(|r| (r.head, r.attr))
            .collect()
    }

    #[test]
    fn scans_title_and_instructions() {
        let n = ht(
            "Review {{ start.invoice_id }}",
            Some("Vendor: {{ start.vendor_name }}"),
            vec![],
        );
        assert_eq!(
            pairs(&n),
            vec![
                ("start".into(), "invoice_id".into()),
                ("start".into(), "vendor_name".into())
            ]
        );
    }

    #[test]
    fn scans_step_blocks_recursively() {
        let step = TaskStepConfig {
            id: "s1".into(),
            title: "Step {{ review.id }}".into(),
            description_mdsvex: Some("for {{ review.vendor }}".into()),
            blocks: vec![
                TaskBlockConfig::Mdsvex {
                    content: "Amount {{ review.amount }}".into(),
                },
                TaskBlockConfig::Callout {
                    severity: CalloutSeverity::Info,
                    title: Some("Heads up {{ review.urgent }}".into()),
                    content: "See {{ review.note }}".into(),
                },
                TaskBlockConfig::Image {
                    filenames: vec![],
                    display: Default::default(),
                    url: Some("{{ scan.url }}".into()),
                    alt: None,
                    caption: Some("Page {{ scan.page }}".into()),
                },
                TaskBlockConfig::Pdf {
                    filename: None,
                    url: Some("{{ pdfdoc.url }}".into()),
                    caption: Some("Doc {{ pdfdoc.name }}".into()),
                    height: None,
                },
            ],
        };
        let n = ht("T", None, vec![step]);
        let pairs = pairs(&n);
        assert!(pairs.contains(&("review".into(), "id".into())));
        assert!(pairs.contains(&("review".into(), "vendor".into())));
        assert!(pairs.contains(&("review".into(), "amount".into())));
        assert!(pairs.contains(&("review".into(), "urgent".into())));
        assert!(pairs.contains(&("review".into(), "note".into())));
        assert!(pairs.contains(&("scan".into(), "url".into())));
        assert!(pairs.contains(&("scan".into(), "page".into())));
        assert!(pairs.contains(&("pdfdoc".into(), "url".into())));
        assert!(pairs.contains(&("pdfdoc".into(), "name".into())));
    }

    #[test]
    fn skips_single_segment_placeholders() {
        // `{{ invoice_id }}` is a slim-token control reference, not a
        // slug-namespaced borrow. The borrow planner must not pick it up.
        let n = ht("Pay {{ invoice_id }}", None, vec![]);
        assert!(pairs(&n).is_empty());
    }

    #[test]
    fn skips_invalid_placeholders() {
        // `{{ a + b }}` doesn't parse; runtime keeps it as literal text.
        let n = ht("Sum {{ a + b }}", Some("unterminated {{ nope"), vec![]);
        assert!(pairs(&n).is_empty());
    }

    #[test]
    fn skips_form_input_field_schema() {
        // `TaskBlockConfig::Input` carries a typed form schema, not
        // markdown — no placeholders to extract from it.
        let step = TaskStepConfig {
            id: "s1".into(),
            title: "S".into(),
            description_mdsvex: None,
            blocks: vec![TaskBlockConfig::Input {
                field: TaskFieldConfig {
                    name: "amount".into(),
                    label: "Amount".into(),
                    kind: TaskFieldKind::Number,
                    required: Some(true),
                    ..TaskFieldConfig::default()
                },
            }],
        };
        let n = ht("T", None, vec![step]);
        assert!(pairs(&n).is_empty());
    }

    #[test]
    fn non_human_task_yields_empty() {
        let n = WorkflowNode {
            id: "x".into(),
            node_type: "start".into(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Start {
                label: "S".into(),
                description: None,
                initial: crate::models::template::default_initial_port(),
                process_name: None,
            },
            parent_id: None,
            width: None,
            height: None,
        };
        assert!(extract_human_task_refs(&n).is_empty());
    }

    #[test]
    fn multiple_placeholders_in_one_string_preserved() {
        let n = ht("{{ a.x }} and {{ b.y }} again {{ a.x }}", None, vec![]);
        assert_eq!(
            pairs(&n),
            vec![
                ("a".into(), "x".into()),
                ("b".into(), "y".into()),
                ("a".into(), "x".into()),
            ],
            "duplicates preserved at scan time; borrow plan dedupes downstream"
        );
    }

    #[test]
    fn chain_emits_only_first_two_segments() {
        // `{{ review.config.timeout }}` — head=review, attr=config.
        // Mirrors python_refs chain handling: the outermost pair is
        // the authored slug+field; deeper segments are attr lookups on
        // the staged producer envelope.
        let n = ht("{{ review.config.timeout }}", None, vec![]);
        assert_eq!(pairs(&n), vec![("review".into(), "config".into())]);
    }

    #[test]
    fn parse_repeater_ref_head_attr_handles_wildcards() {
        // Bare iteration head — sub-form expects whole element.
        assert_eq!(
            parse_repeater_ref_head_attr("extract.tasks[*]"),
            Some(HumanTaskRef {
                head: "extract".into(),
                attr: "tasks".into(),
            })
        );
        // Per-element field — same (head, attr) pair: the borrow target
        // is the parked array, not the inner field.
        assert_eq!(
            parse_repeater_ref_head_attr("extract.tasks[*].title"),
            Some(HumanTaskRef {
                head: "extract".into(),
                attr: "tasks".into(),
            })
        );
        // Whitespace-tolerant.
        assert_eq!(
            parse_repeater_ref_head_attr("  llm.items[*]  "),
            Some(HumanTaskRef {
                head: "llm".into(),
                attr: "items".into(),
            })
        );
        // Bare slug — no field, nothing to borrow.
        assert_eq!(parse_repeater_ref_head_attr("extract"), None);
        // Empty — nothing.
        assert_eq!(parse_repeater_ref_head_attr(""), None);
    }

    #[test]
    fn repeater_block_emits_items_ref_borrow() {
        let step = TaskStepConfig {
            id: "s1".into(),
            title: "Review tasks".into(),
            description_mdsvex: None,
            blocks: vec![TaskBlockConfig::Repeater {
                items_ref: "extract.tasks[*]".into(),
                item_label_ref: Some("extract.tasks[*].title".into()),
                blocks: vec![],
                output_slug: "review_tasks".into(),
            }],
        };
        let n = ht("T", None, vec![step]);
        let pairs = pairs(&n);
        // The Repeater contributes one (extract, tasks) pair per ref; the
        // borrow planner dedupes downstream. `item_label_ref` carries the
        // same (head, attr) so we see it twice.
        assert_eq!(
            pairs
                .iter()
                .filter(|p| p.0 == "extract" && p.1 == "tasks")
                .count(),
            2
        );
    }

    #[test]
    fn repeater_inner_mdsvex_emits_referenced_borrows() {
        // Display blocks inside a Repeater can carry their own
        // `{{ <slug>.<field> }}` placeholders. The borrow planner needs
        // to see those refs so it can synthesize read-arcs on every
        // referenced producer — not just the items_ref head.
        let step = TaskStepConfig {
            id: "s1".into(),
            title: "Review".into(),
            description_mdsvex: None,
            blocks: vec![TaskBlockConfig::Repeater {
                items_ref: "extract.tasks[*]".into(),
                item_label_ref: None,
                blocks: vec![TaskBlockConfig::Mdsvex {
                    content: "Title: {{ extract.tasks[*].title }} for {{ start.vendor }}".into(),
                }],
                output_slug: "review_tasks".into(),
            }],
        };
        let n = ht("T", None, vec![step]);
        let p = pairs(&n);
        // items_ref → (extract, tasks)
        assert!(p.contains(&("extract".into(), "tasks".into())));
        // inner mdsvex `{{ start.vendor }}` → (start, vendor)
        assert!(
            p.contains(&("start".into(), "vendor".into())),
            "expected (start, vendor) from inner Mdsvex, got {p:?}"
        );
    }
}
