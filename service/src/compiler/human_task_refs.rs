//! HumanTask placeholder ref extractor — mirror of [`python_refs`] for
//! authored markdown / interpolated strings.
//!
//! Walks every author-visible string on a `HumanTask` node (title,
//! instructions, every step's title/description, every block's
//! markdown / callout / image-pdf caption / url) and returns each
//! `{{ <head>.<attr> … }}` placeholder's first two path segments. The
//! caller filters by whether `head` matches a known graph slug — exactly
//! the same downstream resolution the Python borrow planner uses
//! (`automated_step_borrow_plan`). One model.
//!
//! Lexical reuse: each candidate placeholder is run through
//! [`parse_placeholder_segments`] so the validation rules (no Rhai
//! expressions, identifier-safe heads, optional numeric indices) are
//! shared byte-for-byte with the runtime accessor builder. A placeholder
//! that doesn't parse is silently skipped — keeping the surface fully
//! permissive on the authored side; the existing rhai_gen pass still
//! leaves bad placeholders as literal text.
//!
//! [`python_refs`]: super::python_refs
//! [`parse_placeholder_segments`]: super::rhai_gen::parse_placeholder_segments

use crate::compiler::rhai_gen::{parse_placeholder_segments, PathSegment};
use crate::models::template::{TaskBlockConfig, TaskStepConfig, WorkflowNode, WorkflowNodeData};

/// One `{{ <head>.<attr> … }}` placeholder site detected on a HumanTask.
/// `head` is the first path segment (a candidate slug); `attr` is the
/// second (the first field off the slug-namespaced producer envelope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanTaskRef {
    pub head: String,
    pub attr: String,
}

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
    out
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
        TaskBlockConfig::Input { .. }
        | TaskBlockConfig::Divider
        | TaskBlockConfig::File { .. } => {}
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
    }
}

/// Scan one free-form string for `{{ … }}` placeholders, push every
/// `(head, attr)` pair where the placeholder validates as a dotted-path
/// accessor with at least two segments.
fn scan_into(raw: &str, out: &mut Vec<HumanTaskRef>) {
    let mut rest = raw;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close_rel) = after.find("}}") else {
            return;
        };
        let inner = &after[..close_rel];
        if let Some(segs) = parse_placeholder_segments(inner) {
            // Need a `head.<attr>` pair to be a candidate borrow.
            // Single-segment placeholders (`{{ invoice_id }}`) still work
            // against the slim control token at runtime — they are not
            // slug-namespaced borrows.
            if let (Some(PathSegment::Field(head)), Some(second)) = (segs.first(), segs.get(1)) {
                let attr = match second {
                    PathSegment::Field(a) => a.clone(),
                    // `{{ start[0].x }}` — an index immediately off the
                    // head isn't a field access on a slug-namespaced
                    // envelope. Skip; the runtime will still try
                    // `__pluck(input, ["start", 0, "x"])` against the
                    // slim token and degrade to `()` if absent.
                    PathSegment::Index(_) => {
                        rest = &after[close_rel + 2..];
                        continue;
                    }
                };
                out.push(HumanTaskRef {
                    head: head.clone(),
                    attr,
                });
            }
        }
        rest = &after[close_rel + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::{
        CalloutSeverity, Position, TaskBlockConfig, TaskFieldConfig, TaskFieldKind,
        TaskStepConfig, WorkflowNode, WorkflowNodeData,
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
                    placeholder: None,
                    options: None,
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
        let n = ht(
            "{{ a.x }} and {{ b.y }} again {{ a.x }}",
            None,
            vec![],
        );
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
}
