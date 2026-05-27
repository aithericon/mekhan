#![allow(unused_imports)]

pub use super::*;
pub use crate::compiler::error::CompileError;
pub use crate::models::template::{FieldKind, JoinMode, MergeStrategy, Port, WorkflowGraph, WorkflowNode, WorkflowNodeData};


#[cfg(test)]
mod port_contract_tests {
    use super::*;
    use crate::models::template::{PortField, Position};
    use serde_json::json;

    fn start_node(fields: Vec<PortField>) -> WorkflowNode {
        WorkflowNode {
            id: "start".to_string(),
            node_type: "start".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Start {
                label: "Start".to_string(),
                description: None,
                initial: Port {
                    id: "in".to_string(),
                    label: "Input".to_string(),
                    fields,
                },
                process_name: None,
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn field(name: &str, kind: FieldKind, required: bool) -> PortField {
        PortField {
            name: name.to_string(),
            label: name.to_string(),
            kind,
            required,
            options: None,
            description: None,
            accept: None,
        }
    }

    fn invoice_port() -> Port {
        Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![
                field("invoice_file", FieldKind::File, true),
                field("invoice_id", FieldKind::Text, true),
            ],
        }
    }

    // The live incident: `invoice_file` resolved to the JSON scalar `"example"`
    // instead of an uploaded file ref. The lenient `Port::validate_token`
    // accepts it (a `file` field accepts a string); the strict SSOT gate must
    // reject it with a field-named, type-specific message — at ingestion,
    // before any net is created.
    #[test]
    fn file_field_as_scalar_string_is_rejected() {
        let node = start_node(invoice_port().fields);
        let token = json!({ "invoice_file": "example", "invoice_id": "example" });
        let v = validate_token_against_port(&invoice_port(), &node, &token)
            .expect_err("a string for a `file` field must be rejected");
        assert_eq!(v.field, "invoice_file");
        assert_eq!(v.actual, "string");
        assert!(
            v.expected.contains("file reference object"),
            "message should name the expected file shape, got: {}",
            v.expected
        );
        // Sanity: the lenient gate is exactly why this slipped to the net.
        assert!(invoice_port().validate_token(&token).is_ok());
    }

    #[test]
    fn valid_uploaded_file_ref_passes() {
        let node = start_node(invoice_port().fields);
        let token = json!({
            "invoice_file": {
                "key": "blob/abc",
                "url": "/api/v1/files/blob/abc",
                "filename": "invoice.png",
                "content_type": "image/png",
                "size": 1234
            },
            "invoice_id": "INV-1"
        });
        assert!(validate_token_against_port(&invoice_port(), &node, &token).is_ok());
    }

    #[test]
    fn number_field_as_string_is_rejected() {
        let port = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![field("amount", FieldKind::Number, true)],
        };
        let node = start_node(port.fields.clone());
        let v = validate_token_against_port(&port, &node, &json!({ "amount": "13" }))
            .expect_err("a string for a `number` field must be rejected");
        assert_eq!(v.field, "amount");
        assert_eq!(v.expected, "number");
        assert_eq!(v.actual, "string");
    }

    #[test]
    fn absent_field_is_not_a_type_error() {
        // Required/absent is `Port::validate_token`'s job — this strict gate
        // is type-only and must stay silent on absence so the two layers
        // compose without double-reporting.
        let node = start_node(invoice_port().fields);
        assert!(
            validate_token_against_port(&invoice_port(), &node, &json!({ "invoice_id": "x" }))
                .is_ok()
        );
    }

    #[test]
    fn non_object_token_is_rejected() {
        let node = start_node(invoice_port().fields);
        let v = validate_token_against_port(&invoice_port(), &node, &json!("not an object"))
            .expect_err("a non-object token cannot satisfy a field-keyed port");
        assert_eq!(v.field, "in");
        assert_eq!(v.actual, "string");
    }

    #[test]
    fn json_escape_hatch_accepts_anything() {
        let port = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![field("blob", FieldKind::Json, true)],
        };
        let node = start_node(port.fields.clone());
        assert!(validate_token_against_port(&port, &node, &json!({ "blob": "anything" })).is_ok());
        assert!(validate_token_against_port(&port, &node, &json!({ "blob": 42 })).is_ok());
    }
}

#[cfg(test)]
mod scope_reachability_tests {
    //! Task #20: the editor picker scope, the read-arc synthesis, and the
    //! drop diagnostic must be three views of ONE borrow-reachable model.
    //! Before the fix, `check-amount` (a Decision sitting after the
    //! token-replacing `extract` automated step) only saw extract's executor
    //! envelope — `review`'s `invoice_amount` was invisible in the picker yet
    //! the compiler happily read-arced it, and `check_guard` flagged it
    //! `DroppedUpstream`: three layers, three answers.
    use super::*;

    fn invoice_graph() -> WorkflowGraph {
        // Same fixture the foundation e2e proves the net enforces — so the
        // picker can't drift from what the compiler binds.
        let s = std::fs::read_to_string("tests/fixtures/graphs/invoice-processing.json")
            .expect("read invoice fixture");
        serde_json::from_str(&s).expect("deser invoice fixture")
    }

    #[test]
    fn decision_scope_agrees_with_readarc_synthesis_and_diagnostics() {
        let g = invoice_graph();
        let report = analyze(&g).expect("analyze");

        // (1) Picker offers the upstream parked producer's field,
        //     producer-namespaced as `review.invoice_amount` — not the
        //     `extract` envelope it physically arrives wrapped in, and not the
        //     provenance-erasing flat `input.invoice_amount`.
        let scope = report.scopes.get("check-amount").expect("decision scope");
        let amt = scope
            .iter()
            .find(|e| e.path == "review.invoice_amount")
            .unwrap_or_else(|| {
                panic!(
                    "review.invoice_amount must be pickable at the decision; offered: {:?}",
                    scope.iter().map(|e| &e.path).collect::<Vec<_>>()
                )
            });
        assert_eq!(amt.producer_node, "review");
        assert_eq!(amt.ty, "Number");
        // The flat, provenance-erasing form is gone.
        assert!(
            !scope.iter().any(|e| e.path == "input.invoice_amount"),
            "borrowed data must be slug-qualified, not flat input.*"
        );

        // (2) The read-arc synthesis resolves the IDENTICAL reference to the
        //     same producer (the compiler-as-borrow-checker).
        let binds = guard_readarc_plan(&g).expect("readarc plan");
        assert!(
            binds.iter().any(|b| b.consumer_node_id == "check-amount"
                && b.referenced == "review.invoice_amount"
                && b.producer_node == "review"),
            "guard_readarc_plan must bind review.invoice_amount -> review, got {:?}",
            binds
                .iter()
                .map(|b| (&b.consumer_node_id, &b.referenced, &b.producer_node))
                .collect::<Vec<_>>()
        );

        // (3) No diagnostic contradicts the compiler.
        for d in &report.diagnostics {
            if let ShapeDiagnostic::DroppedUpstream { referenced, .. }
            | ShapeDiagnostic::UnresolvedGuardPath { referenced, .. } = d
            {
                assert_ne!(
                    referenced, "review.invoice_amount",
                    "borrow-reachable ref wrongly flagged dropped/unresolved"
                );
            }
        }

        // Global invariant: nothing the picker offers is, at that same node,
        // reported unresolved — the picker never lies about resolvability.
        for (nid, entries) in &report.scopes {
            for e in entries {
                let contradicted = report.diagnostics.iter().any(|d| matches!(d,
                    ShapeDiagnostic::UnresolvedGuardPath { node_id, referenced, .. }
                        if node_id == nid && referenced == &e.path));
                assert!(
                    !contradicted,
                    "picker offered {} at {} but it is reported unresolved",
                    e.path, nid
                );
            }
        }
    }

    /// Two upstream parked producers contributing the *same* leaf no longer
    /// collapse to one nearest-wins entry (the #20 regression): producer
    /// namespacing makes them distinct paths, an unqualified `input.<key>`
    /// is unbindable (must qualify), and a nearer non-parked node never masks
    /// a farther parked one.
    fn two_producer_graph(decision_guard: &str) -> WorkflowGraph {
        // Start → reviewA → reviewB → decision. Both human tasks emit a form
        // field `amount`; `reviewA` is the *farther* parked producer.
        let step = |field: &str| {
            format!(
                r#"{{"id":"s","title":"S","blocks":[{{"type":"input","field":{{"name":"{field}","label":"Amt","kind":"number","required":true}}}}]}}"#
            )
        };
        let ht = |id: &str, slug: &str| {
            format!(
                r#"{{"id":"{id}","type":"human_task","slug":"{slug}","position":{{"x":0,"y":0}},"data":{{"type":"human_task","label":"{id}","taskTitle":"{id}","steps":[{}]}}}}"#,
                step("amount")
            )
        };
        let json = format!(
            r#"{{"nodes":[
              {{"id":"start","type":"start","position":{{"x":0,"y":0}},"data":{{"type":"start","label":"Start"}}}},
              {ha},
              {hb},
              {{"id":"dec","type":"decision","position":{{"x":0,"y":0}},"data":{{"type":"decision","label":"D","conditions":[{{"edgeId":"hi","label":"hi","guard":"{decision_guard}"}}],"defaultBranch":"default"}}}},
              {{"id":"end1","type":"end","position":{{"x":0,"y":0}},"data":{{"type":"end","label":"E1"}}}},
              {{"id":"end2","type":"end","position":{{"x":0,"y":0}},"data":{{"type":"end","label":"E2"}}}}
            ],"edges":[
              {{"id":"e1","source":"start","target":"reviewA","type":"sequence"}},
              {{"id":"e2","source":"reviewA","target":"reviewB","type":"sequence"}},
              {{"id":"e3","source":"reviewB","target":"dec","type":"sequence"}},
              {{"id":"e4","source":"dec","target":"end1","sourceHandle":"hi","type":"sequence"}},
              {{"id":"e5","source":"dec","target":"end2","sourceHandle":"default","type":"sequence"}}
            ]}}"#,
            ha = ht("reviewA", "rev_a"),
            hb = ht("reviewB", "rev_b"),
        );
        serde_json::from_str(&json).expect("deser two-producer graph")
    }

    #[test]
    fn collision_distinct_parked_producers_get_distinct_qualified_paths() {
        let g = two_producer_graph("rev_a.amount > 0");
        let report = analyze(&g).expect("analyze");
        let scope = report.scopes.get("dec").expect("decision scope");
        let paths: std::collections::BTreeSet<&str> =
            scope.iter().map(|e| e.path.as_str()).collect();

        // Same key, two parked owners → two DISTINCT producer-namespaced
        // entries (no nearest-wins collapse, no silent loss).
        assert!(
            paths.contains("rev_a.amount") && paths.contains("rev_b.amount"),
            "both producers' `amount` must be distinctly pickable, got: {paths:?}"
        );
        let a = scope.iter().find(|e| e.path == "rev_a.amount").unwrap();
        let b = scope.iter().find(|e| e.path == "rev_b.amount").unwrap();
        assert_eq!(a.producer_node, "reviewA");
        assert_eq!(b.producer_node, "reviewB");
        // The flat form that erased the producer is gone entirely.
        assert!(
            !paths.contains("input.amount"),
            "unqualified borrowed key must not be offered: {paths:?}"
        );

        // The qualified guard binds to its named producer — the farther one,
        // proving a nearer parked/forwarding node does not mask it.
        let binds = guard_readarc_plan(&g).expect("readarc plan");
        assert!(
            binds
                .iter()
                .any(|x| x.referenced == "rev_a.amount" && x.producer_node == "reviewA"),
            "rev_a.amount must bind to reviewA, got {:?}",
            binds
                .iter()
                .map(|x| (&x.referenced, &x.producer_node))
                .collect::<Vec<_>>()
        );

        // An unqualified, non-control `input.amount` is unbindable: hard
        // error at compile, naming the qualified forms to use; and the same
        // node reports it unresolved for the editor.
        let g2 = two_producer_graph("input.amount > 0");
        match guard_readarc_plan(&g2) {
            Err(CompileError::GuardUnresolved {
                node_id,
                identifier,
                available,
            }) => {
                assert_eq!(node_id, "dec");
                assert_eq!(identifier, "input.amount");
                assert!(
                    available.iter().any(|p| p == "rev_a.amount")
                        && available.iter().any(|p| p == "rev_b.amount"),
                    "the error must name both qualified forms, got: {available:?}"
                );
            }
            other => panic!("expected GuardUnresolved, got {other:?}"),
        }
        let report2 = analyze(&g2).expect("analyze g2");
        assert!(
            report2.diagnostics.iter().any(|d| matches!(d,
                ShapeDiagnostic::UnresolvedGuardPath { node_id, referenced, .. }
                    if node_id == "dec" && referenced == "input.amount")),
            "editor must see input.amount as unresolved at the decision"
        );
    }

    /// Start is a parked producer (`lower.rs::park_outputs`): its declared
    /// inputs are borrow-reachable downstream as `<start-slug>.<field>`,
    /// exactly like a human task's, and genuine control/identity leaves are
    /// attributed to the synthetic "Process" group instead of whichever node
    /// last forwarded the token (the `input.status`-under-Extract-Data bug).
    fn start_producer_graph(decision_guard: &str) -> WorkflowGraph {
        let v = serde_json::json!({
            "nodes": [
                {"id":"start","type":"start","position":{"x":0,"y":0},
                 "data":{"type":"start","label":"Start",
                    "initial":{"id":"in","label":"Intake","fields":[
                        {"name":"note","label":"Note","kind":"text","required":true}]}}},
                {"id":"dec","type":"decision","position":{"x":0,"y":0},
                 "data":{"type":"decision","label":"D",
                    "conditions":[{"edgeId":"hi","label":"hi","guard":decision_guard}],
                    "defaultBranch":"default"}},
                {"id":"end1","type":"end","position":{"x":0,"y":0},"data":{"type":"end","label":"E1"}},
                {"id":"end2","type":"end","position":{"x":0,"y":0},"data":{"type":"end","label":"E2"}}
            ],
            "edges": [
                {"id":"e1","source":"start","target":"dec","type":"sequence"},
                {"id":"e4","source":"dec","target":"end1","sourceHandle":"hi","type":"sequence"},
                {"id":"e5","source":"dec","target":"end2","sourceHandle":"default","type":"sequence"}
            ]
        });
        serde_json::from_value(v).expect("deser start-producer graph")
    }

    #[test]
    fn start_is_parked_producer_and_control_leaves_grouped_as_process() {
        let g = start_producer_graph("start.note == \"ok\"");
        let report = analyze(&g).expect("analyze");
        let scope = report.scopes.get("dec").expect("decision scope");

        // (1) Start's declared input is borrow-reachable, namespaced by the
        //     Start's slug (derived from node id `start`) — never flat.
        let note = scope.iter().find(|e| e.path == "start.note").unwrap_or_else(|| {
            panic!(
                "start.note must be pickable at the decision; offered: {:?}",
                scope.iter().map(|e| &e.path).collect::<Vec<_>>()
            )
        });
        assert_eq!(note.producer_node, "start");
        assert_eq!(note.ty, "String");
        assert!(
            !scope.iter().any(|e| e.path == "input.note"),
            "Start data must be slug-qualified, not flat input.*"
        );

        // (2) Genuine control/identity leaves (`_instance_id`) go to the
        //     synthetic "Process" group, not a business producer.
        let proc = scope
            .iter()
            .find(|e| e.path == "input._instance_id")
            .expect("control leaf input._instance_id must be offered");
        assert_eq!(proc.producer_label, "Process");
        assert_eq!(proc.producer_node, "");
        assert!(
            !scope
                .iter()
                .any(|e| e.path.starts_with("input.") && e.producer_label != "Process"),
            "every control leaf must group under Process, got {:?}",
            scope
                .iter()
                .map(|e| (&e.path, &e.producer_label))
                .collect::<Vec<_>>()
        );

        // (3) The read-arc synthesis binds the IDENTICAL ref to the Start's
        //     parked data place (`apply_control_data_foundation` borrows
        //     `p_start_data`) — picker == compiler.
        let binds = guard_readarc_plan(&g).expect("readarc plan");
        assert!(
            binds.iter().any(|b| b.consumer_node_id == "dec"
                && b.referenced == "start.note"
                && b.producer_node == "start"),
            "guard_readarc_plan must bind start.note -> start, got {:?}",
            binds
                .iter()
                .map(|b| (&b.consumer_node_id, &b.referenced, &b.producer_node))
                .collect::<Vec<_>>()
        );

        // (4) The picker never lies: nothing it offers is, at that node,
        //     reported unresolved.
        for e in scope {
            let contradicted = report.diagnostics.iter().any(|d| matches!(d,
                ShapeDiagnostic::UnresolvedGuardPath { node_id, referenced, .. }
                    if node_id == "dec" && referenced == &e.path));
            assert!(
                !contradicted,
                "picker offered {} but it is reported unresolved",
                e.path
            );
        }
    }

    /// A Python AutomatedStep that reads `review.invoice_amount` in its
    /// source must produce exactly one [`AutomatedStepDataBorrow`] from
    /// the consumer (the AutomatedStep) to the producer (the upstream
    /// HumanTask `review`) — the same borrow-checker model the
    /// Decision/Loop branch already uses, just sourced from Python AST
    /// instead of Rhai.
    #[test]
    fn python_automated_step_review_field_emits_borrow() {
        use std::collections::HashMap;

        // Start → review (HumanTask, slug "review", produces `invoice_amount`)
        //       → extract (Python AutomatedStep) → end
        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"review","type":"human_task","slug":"review","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review","taskTitle":"Review",
                     "steps":[{"id":"s","title":"S","blocks":[
                       {"type":"input","field":{"name":"invoice_amount","label":"Amt","kind":"number","required":true}}
                     ]}]}},
            {"id":"extract","type":"automated_step","slug":"extract","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Extract",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"review","type":"sequence"},
            {"id":"e2","source":"review","target":"extract","type":"sequence"},
            {"id":"e3","source":"extract","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser borrow graph");

        let mut inline: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
            HashMap::new();
        let mut step_files = HashMap::new();
        step_files.insert(
            "main.py".to_string(),
            "amount = review.invoice_amount\nprint(amount)\n".to_string(),
        );
        inline.insert("extract".to_string(), step_files);

        let borrows = automated_step_borrow_plan(&g, &inline).expect("borrow plan");
        assert_eq!(
            borrows.len(),
            1,
            "exactly one borrow expected; got: {borrows:?}"
        );
        assert_eq!(borrows[0].consumer_node_id(), "extract");
        assert_eq!(borrows[0].slug(), "review");
        assert_eq!(borrows[0].producer_node(), "review");
    }

    /// Multiple accesses to the SAME producer collapse to one borrow per
    /// `(consumer, producer)` pair — the runtime stages the whole
    /// envelope once and the user reads any number of fields off it.
    #[test]
    fn python_borrow_dedupes_per_producer() {
        use std::collections::HashMap;

        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"review","type":"human_task","slug":"review","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review","taskTitle":"Review",
                     "steps":[{"id":"s","title":"S","blocks":[
                       {"type":"input","field":{"name":"invoice_amount","label":"Amt","kind":"number","required":true}},
                       {"type":"input","field":{"name":"vendor_name","label":"V","kind":"text","required":true}}
                     ]}]}},
            {"id":"extract","type":"automated_step","slug":"extract","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Extract",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"review","type":"sequence"},
            {"id":"e2","source":"review","target":"extract","type":"sequence"},
            {"id":"e3","source":"extract","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser borrow graph");

        let mut inline: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut step_files = HashMap::new();
        step_files.insert(
            "main.py".to_string(),
            "a = review.invoice_amount\nb = review.vendor_name\nc = review.invoice_amount\n"
                .to_string(),
        );
        inline.insert("extract".to_string(), step_files);

        let borrows = automated_step_borrow_plan(&g, &inline).expect("borrow plan");
        // Three accesses on `review` → one borrow.
        assert_eq!(
            borrows.len(),
            1,
            "borrow plan must dedupe per (consumer, producer); got: {borrows:?}"
        );
    }

    /// An identifier that isn't a known slug (stdlib module, local var,
    /// typo) is silently ignored — no borrow, no hard error, no false
    /// positive against `os.path` and friends.
    #[test]
    fn python_unknown_head_is_silently_ignored() {
        use std::collections::HashMap;

        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"extract","type":"automated_step","slug":"extract","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Extract",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"extract","type":"sequence"},
            {"id":"e2","source":"extract","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser graph");

        let mut inline: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut step_files = HashMap::new();
        step_files.insert(
            "main.py".to_string(),
            "import os\np = os.path.join('a', 'b')\nlocal_var = {'k': 1}\nv = local_var.get('k')\n"
                .to_string(),
        );
        inline.insert("extract".to_string(), step_files);

        let borrows = automated_step_borrow_plan(&g, &inline).expect("borrow plan");
        assert!(
            borrows.is_empty(),
            "stdlib + locals must not become borrows; got: {borrows:?}"
        );
    }

    /// One model: a HumanTask's `{{ <slug>.<field> }}` placeholder
    /// resolves to a single borrow against the upstream parked place,
    /// exactly like a Python AutomatedStep's `<slug>.<field>` source
    /// access.
    #[test]
    fn human_task_borrow_simple() {
        // Start(slug=start, with invoice_id) → review (HumanTask) → end.
        // The HumanTask title interpolates `{{ start.invoice_id }}`.
        let json = r#"{
          "nodes":[
            {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start",
                     "initial":{"id":"in","label":"Initial","fields":[
                       {"name":"invoice_id","label":"Invoice","kind":"text","required":true}
                     ]}}},
            {"id":"review","type":"human_task","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review",
                     "taskTitle":"Review {{ start.invoice_id }}",
                     "steps":[]}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"s","target":"review","type":"sequence"},
            {"id":"e2","source":"review","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser borrow graph");
        let borrows = human_task_borrow_plan(&g).expect("borrow plan");
        assert_eq!(borrows.len(), 1, "expected exactly one borrow; got: {borrows:?}");
        assert_eq!(borrows[0].consumer_node_id, "review");
        assert_eq!(borrows[0].slug, "start");
        assert_eq!(borrows[0].producer_node, "s");
    }

    /// Multiple placeholders against the same producer collapse to one
    /// borrow per `(consumer, producer)` pair — mirrors the Python
    /// dedupe rule. The runtime read-arc reaches the whole envelope, the
    /// Rhai `__pluck` walks down to the individual field per call site.
    #[test]
    fn human_task_borrow_dedupes_per_producer() {
        let json = r#"{
          "nodes":[
            {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start",
                     "initial":{"id":"in","label":"Initial","fields":[
                       {"name":"invoice_id","label":"I","kind":"text","required":true},
                       {"name":"vendor_name","label":"V","kind":"text","required":true}
                     ]}}},
            {"id":"review","type":"human_task","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review",
                     "taskTitle":"Pay {{ start.vendor_name }} for {{ start.invoice_id }}",
                     "instructionsMdsvex":"Re: {{ start.invoice_id }}",
                     "steps":[]}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"s","target":"review","type":"sequence"},
            {"id":"e2","source":"review","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser borrow graph");
        let borrows = human_task_borrow_plan(&g).expect("borrow plan");
        assert_eq!(
            borrows.len(),
            1,
            "three placeholders on `start` → one borrow; got: {borrows:?}"
        );
    }

    /// An unknown head identifier (typo, root-level control-token
    /// field like `{{ status }}`, or a placeholder pointing nowhere)
    /// is silently ignored — same posture as Python's
    /// `python_unknown_head_is_silently_ignored`. The interpolation
    /// stays in place and `__pluck` degrades to `()` at runtime.
    #[test]
    fn human_task_unknown_slug_ignored() {
        let json = r#"{
          "nodes":[
            {"id":"s","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"review","type":"human_task","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review",
                     "taskTitle":"{{ mystery.field }} or {{ also_unknown }}",
                     "steps":[]}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"s","target":"review","type":"sequence"},
            {"id":"e2","source":"review","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser graph");
        let borrows = human_task_borrow_plan(&g).expect("borrow plan");
        assert!(
            borrows.is_empty(),
            "unknown slugs and root-level placeholders must not become borrows; got: {borrows:?}"
        );
    }

    // ──────────────────────────────────────────────────────────────────
    // LLM / Kreuzberg borrow planner tests
    // ──────────────────────────────────────────────────────────────────

    /// Fixture: Start → review (HumanTask: invoice_amount:number, vendor_name:text)
    ///          → ocr_step (Kreuzberg, attached PDF, outputs content:text)
    ///          → classify (LLM, prompt references {{review.vendor_name}} +
    ///                      {{ocr_step.content}})
    ///          → end
    fn ocr_classify_graph(prompt: &str) -> WorkflowGraph {
        let json = format!(
            r#"{{
              "nodes": [
                {{"id":"s","type":"start","slug":"start","position":{{"x":0,"y":0}},
                 "data":{{"type":"start","label":"Start"}}}},
                {{"id":"review","type":"human_task","slug":"review","position":{{"x":0,"y":0}},
                 "data":{{"type":"human_task","label":"Review","taskTitle":"R",
                         "steps":[{{"id":"s1","title":"S","blocks":[
                           {{"type":"input","field":{{"name":"invoice_amount","label":"A","kind":"number","required":true}}}},
                           {{"type":"input","field":{{"name":"vendor_name","label":"V","kind":"text","required":true}}}},
                           {{"type":"input","field":{{"name":"invoice_pdf","label":"P","kind":"file","required":true}}}}
                         ]}}]}}}},
                {{"id":"ocr_step","type":"automated_step","slug":"ocr_step","position":{{"x":0,"y":0}},
                 "data":{{"type":"automated_step","label":"OCR",
                         "executionSpec":{{"backendType":"kreuzberg","config":{{"file":"sample.pdf"}}}},
                         "retryPolicy":{{"maxRetries":0,"strategy":{{"type":"immediate"}}}},
                         "deploymentModel":{{"mode":"inline"}},
                         "output":{{"id":"out","label":"out","fields":[
                           {{"name":"content","label":"Content","kind":"text","required":true}}
                         ]}}}}}},
                {{"id":"classify","type":"automated_step","slug":"classify","position":{{"x":0,"y":0}},
                 "data":{{"type":"automated_step","label":"Classify",
                         "executionSpec":{{"backendType":"llm","config":{{
                            "provider":"openai","model":"gpt-4o-mini",
                            "prompt":{prompt}
                         }}}},
                         "retryPolicy":{{"maxRetries":0,"strategy":{{"type":"immediate"}}}},
                         "deploymentModel":{{"mode":"inline"}},
                         "output":{{"id":"out","label":"out","fields":[
                           {{"name":"klass","label":"K","kind":"text","required":true}}
                         ]}}}}}},
                {{"id":"end","type":"end","position":{{"x":0,"y":0}},
                 "data":{{"type":"end","label":"End"}}}}
              ],
              "edges":[
                {{"id":"e1","source":"s","target":"review","type":"sequence"}},
                {{"id":"e2","source":"review","target":"ocr_step","type":"sequence"}},
                {{"id":"e3","source":"ocr_step","target":"classify","type":"sequence"}},
                {{"id":"e4","source":"classify","target":"end","type":"sequence"}}
              ]
            }}"#,
            prompt = prompt
        );
        serde_json::from_str(&json).expect("deser ocr_classify graph")
    }

    #[test]
    fn llm_prompt_simple_borrow() {
        use std::collections::HashMap;
        let g = ocr_classify_graph(r#""Classify: {{ ocr_step.content }} for {{ review.vendor_name }}""#);
        let borrows = automated_step_borrow_plan(&g, &HashMap::new()).expect("borrow plan");

        let pairs: Vec<(String, String)> = borrows
            .iter()
            .filter_map(|b| match b {
                AutomatedStepDataBorrow::PerField {
                    consumer_node_id,
                    slug,
                    attr,
                    ..
                } if consumer_node_id == "classify" => Some((slug.clone(), attr.clone())),
                _ => None,
            })
            .collect();
        assert!(pairs.contains(&("ocr_step".into(), "content".into())));
        assert!(pairs.contains(&("review".into(), "vendor_name".into())));
        // All classify borrows must be content sites (is_path_site=false) —
        // the prompt is a content surface.
        for b in &borrows {
            if let AutomatedStepDataBorrow::PerField {
                consumer_node_id,
                is_path_site,
                ..
            } = b
            {
                if consumer_node_id == "classify" {
                    assert!(!*is_path_site, "prompt site must be content (is_path_site=false)");
                }
            }
        }
    }

    #[test]
    fn llm_unknown_slug_is_hard_error() {
        use std::collections::HashMap;
        let g = ocr_classify_graph(r#""Classify: {{ typo_slug.content }}""#);
        let err = automated_step_borrow_plan(&g, &HashMap::new())
            .expect_err("unknown slug must error");
        match err {
            CompileError::BackendRefUnresolved {
                backend,
                kind,
                name,
                slug,
                ..
            } => {
                assert_eq!(backend, "llm");
                assert_eq!(kind, "slug");
                assert_eq!(name, "typo_slug");
                assert_eq!(slug, "typo_slug");
            }
            other => panic!("expected BackendRefUnresolved(slug), got {other:?}"),
        }
    }

    #[test]
    fn llm_unknown_field_on_known_slug_is_hard_error() {
        use std::collections::HashMap;
        let g = ocr_classify_graph(r#""Classify: {{ ocr_step.no_such_field }}""#);
        let err = automated_step_borrow_plan(&g, &HashMap::new())
            .expect_err("unknown field must error");
        match err {
            CompileError::BackendRefUnresolved {
                kind,
                name,
                slug,
                field,
                available,
                ..
            } => {
                assert_eq!(kind, "field");
                assert_eq!(name, "no_such_field");
                assert_eq!(slug, "ocr_step");
                assert_eq!(field, "no_such_field");
                assert!(
                    available.contains(&"content".to_string()),
                    "available fields must include 'content', got {available:?}"
                );
            }
            other => panic!("expected BackendRefUnresolved(field), got {other:?}"),
        }
    }

    #[test]
    fn llm_content_site_rejects_file_kind_producer() {
        use std::collections::HashMap;
        // Interpolating a File-kind upstream into a text prompt is nonsense.
        let g = ocr_classify_graph(r#""Inline PDF? {{ review.invoice_pdf }}""#);
        let err = automated_step_borrow_plan(&g, &HashMap::new())
            .expect_err("file-kind in prompt must error");
        assert!(matches!(err, CompileError::LlmImageRefNotFileKind { .. }));
    }

    #[test]
    fn llm_no_placeholders_yields_no_borrows() {
        use std::collections::HashMap;
        let g = ocr_classify_graph(r#""Just a static prompt, no placeholders""#);
        let borrows = automated_step_borrow_plan(&g, &HashMap::new()).expect("borrow plan");
        // No borrows for the classify LLM consumer specifically.
        let classify_borrows: Vec<_> = borrows
            .iter()
            .filter(|b| b.consumer_node_id() == "classify")
            .collect();
        assert!(classify_borrows.is_empty(), "got: {classify_borrows:?}");
    }

    #[test]
    fn kreuzberg_borrow_resolves_file_kind() {
        // Two AutomatedSteps:
        //   1. uploader (HumanTask, slug=uploader, file field "pdf")
        //   2. ocr (Kreuzberg, file: "{{uploader.pdf}}")
        let json = r#"{
          "nodes": [
            {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"uploader","type":"human_task","slug":"uploader","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"U","taskTitle":"U",
                     "steps":[{"id":"s1","title":"S","blocks":[
                       {"type":"input","field":{"name":"pdf","label":"P","kind":"file","required":true}}
                     ]}]}},
            {"id":"ocr","type":"automated_step","slug":"ocr","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"OCR",
                     "executionSpec":{"backendType":"kreuzberg","config":{"file":"{{ uploader.pdf }}"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"s","target":"uploader","type":"sequence"},
            {"id":"e2","source":"uploader","target":"ocr","type":"sequence"},
            {"id":"e3","source":"ocr","target":"end","type":"sequence"}
          ]
        }"#;
        use std::collections::HashMap;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser graph");
        let borrows = automated_step_borrow_plan(&g, &HashMap::new()).expect("borrow plan");
        assert_eq!(borrows.len(), 1, "got: {borrows:?}");
        match &borrows[0] {
            AutomatedStepDataBorrow::PerField {
                consumer_node_id,
                slug,
                producer_node,
                attr,
                is_path_site,
                producer_field_kind,
            } => {
                assert_eq!(consumer_node_id, "ocr");
                assert_eq!(slug, "uploader");
                assert_eq!(producer_node, "uploader");
                assert_eq!(attr, "pdf");
                assert!(*is_path_site);
                assert_eq!(*producer_field_kind, crate::models::template::FieldKind::File);
            }
            other => panic!("Kreuzberg borrow must be PerField, got {other:?}"),
        }
    }

    #[test]
    fn kreuzberg_allows_text_kind_fields() {
        // Kreuzberg over an LLM's text output — temp-file path of the
        // stringified content. Compiler accepts; foundation pass handles
        // the Raw staging.
        let json = r#"{
          "nodes": [
            {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"genreport","type":"automated_step","slug":"genreport","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Gen",
                     "executionSpec":{"backendType":"llm","config":{"provider":"openai","model":"x","prompt":"hello"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"},
                     "output":{"id":"out","label":"out","fields":[
                       {"name":"narrative","label":"N","kind":"text","required":true}
                     ]}}},
            {"id":"reocr","type":"automated_step","slug":"reocr","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"ReOCR",
                     "executionSpec":{"backendType":"kreuzberg","config":{"file":"{{ genreport.narrative }}"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"s","target":"genreport","type":"sequence"},
            {"id":"e2","source":"genreport","target":"reocr","type":"sequence"},
            {"id":"e3","source":"reocr","target":"end","type":"sequence"}
          ]
        }"#;
        use std::collections::HashMap;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser graph");
        let borrows = automated_step_borrow_plan(&g, &HashMap::new()).expect("borrow plan");
        assert_eq!(borrows.len(), 1);
        match &borrows[0] {
            AutomatedStepDataBorrow::PerField {
                producer_field_kind,
                ..
            } => assert_eq!(*producer_field_kind, crate::models::template::FieldKind::Text),
            other => panic!("Kreuzberg borrow must be PerField, got {other:?}"),
        }
    }

    /// File envelope nesting: a Start field `document: File` must surface
    /// downstream as *both* a `FileRef` leaf (`start.document`, what Kreuzberg
    /// and LLM borrow) *and* its three metadata subkeys (`start.document.url`,
    /// `.filename`, `.content_type`, the dotted form HumanTask blocks
    /// interpolate). Before the picker fix, the container leaf was missing
    /// and the subkeys were truncated to `start.{url,filename,content_type}`.
    #[test]
    fn file_envelope_exposes_container_leaf_and_nested_subkeys() {
        let json = r#"{
          "nodes": [
            {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start",
                     "initial":{"id":"in","label":"Input","fields":[
                       {"name":"document","label":"Doc","kind":"file","required":true}
                     ]}}},
            {"id":"ocr","type":"automated_step","slug":"ocr","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"OCR",
                     "executionSpec":{"backendType":"kreuzberg","config":{"file":"{{ start.document }}"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"s","target":"ocr","type":"sequence"},
            {"id":"e2","source":"ocr","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser file-envelope graph");
        let report = analyze(&g).expect("analyze");
        let scope = report.scopes.get("ocr").expect("ocr scope");
        let by_path: std::collections::BTreeMap<&str, &str> =
            scope.iter().map(|e| (e.path.as_str(), e.ty.as_str())).collect();

        assert_eq!(
            by_path.get("start.document").copied(),
            Some("FileRef"),
            "container leaf must be a pickable FileRef; offered: {:?}",
            by_path
        );
        assert_eq!(
            by_path.get("start.document.url").copied(),
            Some("String"),
            "metadata subkey `url` must be nested under the file field, not flat at `start.url`; offered: {:?}",
            by_path
        );
        assert_eq!(by_path.get("start.document.filename").copied(), Some("String"));
        assert_eq!(by_path.get("start.document.content_type").copied(), Some("String"));

        // The pre-fix flat form (the bug from the screenshot) must be gone:
        // `start.url` would imply Start declared a top-level `url` field.
        assert!(
            !by_path.contains_key("start.url"),
            "flat `start.url` must not be offered — that path lives under `document`: {:?}",
            by_path
        );
        assert!(!by_path.contains_key("start.filename"));
        assert!(!by_path.contains_key("start.content_type"));
    }
}
