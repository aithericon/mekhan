//! Make a normally-compiled child template spawn-callable.
//!
//! A child workflow template compiles (via [`compile_to_scenario`]) to a
//! self-contained net with a `Start` entry place (`p_{start}_ready`) and one
//! or more `terminal` places — it has *no* cross-net boundary. To invoke it
//! through the engine's `spawn_net` machinery (the same mechanism
//! [`aithericon_sdk::Context::spawn`] uses) the child must expose the fixed
//! boundary the parent's [`lower_subworkflow`] wires against:
//!
//! - `inbox`  — `bridge_in`: the parent's mapped initial token arrives here
//!   and is forwarded into the child's Start entry place.
//! - `reply_out` — `bridge_reply`: the child's terminal result is forwarded
//!   back to the parent instance via `ReplyRouting.reply_to` (set by the
//!   parent's `bridge_out` `reply_to`).
//! - `fail_out` — `bridge_out` targeting `$params.parent_net_id` /
//!   `$params.failure_place` (the spawn handler injects both): an explicit
//!   child failure routes back to the parent's error path.
//!
//! [`make_child_callable`] performs this transform on the typed
//! [`ScenarioDefinition`] (no JSON surgery), producing byte-compatible
//! boundary places matching the SDK `bridge_in` / `bridge_reply` /
//! `bridge_out_param` builders.
//!
//! ## Lifecycle note
//!
//! The child's `terminal` places are flipped to plain `state` and their token
//! is forwarded to `reply_out`. The child therefore completes by *delivering
//! its reply then going quiescent* (it no longer emits `NetCompleted`) —
//! exactly the proven `spawn_demo` semantic, where the engine's hibernation
//! reclaims the spent child. This is the deliberate resolution of the
//! "terminal → reply ordering" risk: we never race a terminal token against
//! `NetCompleted`; the (former) terminal token is consumed straight onto the
//! reply bridge.

use aithericon_sdk::scenario::{
    BridgeTargetDto, ScenarioArc, ScenarioDefinition, ScenarioPlace, ScenarioPort,
    ScenarioTransition, TransitionLogic,
};

use crate::compiler::CompileError;
use crate::models::template::{FieldKind, Port, PortField, WorkflowGraph, WorkflowNodeData};

/// Fixed boundary place ids — the contract the parent's `lower_subworkflow`
/// wires against (`bridge_in_from(child, "reply_out")`, `bridge_out … "inbox"`,
/// `bridge_in_from(child, "fail_out")`).
pub const CHILD_INBOX: &str = "inbox";
pub const CHILD_REPLY_OUT: &str = "reply_out";
pub const CHILD_FAIL_OUT: &str = "fail_out";

fn port(name: &str) -> ScenarioPort {
    ScenarioPort {
        name: name.to_string(),
        schema_ref: None,
        cardinality: "single".to_string(),
    }
}

fn arc(place: &str, port: &str) -> ScenarioArc {
    ScenarioArc {
        place: place.to_string(),
        port: port.to_string(),
        weight: 1,
        read: false,
        count_from: None,
        correlate_on: None,
    }
}

fn boundary_place(id: &str, name: &str, place_type: &str) -> ScenarioPlace {
    ScenarioPlace {
        id: id.to_string(),
        name: name.to_string(),
        place_type: place_type.to_string(),
        group_id: None,
        capacity: None,
        initial_tokens: vec![],
        // Permissive: bridge boundary carries the dynamic workflow token. The
        // child's *internal* places keep their own schemas; skipping injection
        // validation on the boundary matches the DynamicToken-permissive intent
        // and avoids cross-net definition merging.
        token_schema: None,
        bridge_out: None,
        bridge_reply: false,
        bridge_reply_channel: None,
        bridge_in: None,
    }
}

/// Forwarding transition: consume `from`(port "tok") → produce `to`(port
/// "tok"), identity logic. Used for inbox→Start and terminal→reply_out.
fn forward_transition(id: &str, name: &str, from_place: &str, to_place: &str) -> ScenarioTransition {
    ScenarioTransition {
        id: id.to_string(),
        name: name.to_string(),
        group_id: None,
        input_ports: vec![port("tok")],
        output_ports: vec![port("tok")],
        inputs: vec![arc(from_place, "tok")],
        outputs: vec![arc(to_place, "tok")],
        guard: None,
        priority: None,
        logic: TransitionLogic::Rhai {
            source: "#{ tok: tok }".to_string(),
        },
        effect_config: None,
        caused_signals: vec![],
        input_schema: None,
        output_schema: None,
        process_step_started: None,
        process_step_completed: None,
    }
}

/// Transform `child` in place into a spawn-callable net.
///
/// `start_entry_place_id` is the child's Start entry place (`p_{start}_ready`,
/// stable through the pipeline). `terminal_place_ids` are the child's terminal
/// places (post-merge ids, from the parent compile's `PostProcess`). Errors if
/// the child has no terminal (a sub-workflow with no End can never reply) or
/// the entry place is missing.
pub fn make_child_callable(
    child: &mut ScenarioDefinition,
    start_entry_place_id: &str,
    terminal_place_ids: &[String],
) -> Result<(), CompileError> {
    if !child.places.iter().any(|p| p.id == start_entry_place_id) {
        return Err(CompileError::Compilation(format!(
            "make_child_callable: child has no Start entry place '{start_entry_place_id}'"
        )));
    }
    if terminal_place_ids.is_empty() {
        return Err(CompileError::Compilation(
            "make_child_callable: child template declares no terminal (End) — a \
             sub-workflow must reach an End to return a result"
                .to_string(),
        ));
    }
    // Guard against re-entry / id collisions.
    if child.places.iter().any(|p| p.id == CHILD_INBOX) {
        return Err(CompileError::Compilation(
            "make_child_callable: child already has an 'inbox' place (collision)".to_string(),
        ));
    }

    // 1. inbox (bridge_in) → Start entry.
    child
        .places
        .push(boundary_place(CHILD_INBOX, "Inbox", "bridge_in"));
    child.transitions.push(forward_transition(
        "__sub_inbox",
        "Sub-workflow Inbox",
        CHILD_INBOX,
        start_entry_place_id,
    ));

    // 2. reply_out — a plain state place flagged `bridge_reply` (exactly what
    //    the SDK `bridge_reply` builder emits): tokens produced here route back
    //    to the parent via the consumed token's `reply_routing.reply_to`.
    let mut reply = boundary_place(CHILD_REPLY_OUT, "Reply", "state");
    reply.bridge_reply = true;
    child.places.push(reply);

    // 3. Each terminal → reply_out. Flip the terminal to plain `state` so the
    //    token is consumed straight onto the reply bridge (no NetCompleted
    //    race); the child replies then goes quiescent.
    for (i, tid) in terminal_place_ids.iter().enumerate() {
        if let Some(p) = child.places.iter_mut().find(|p| &p.id == tid) {
            if p.place_type == "terminal" {
                p.place_type = "state".to_string();
            }
        } else {
            return Err(CompileError::Compilation(format!(
                "make_child_callable: terminal place '{tid}' not found in child"
            )));
        }
        child.transitions.push(forward_transition(
            &format!("__sub_reply_{i}"),
            "Sub-workflow Reply",
            tid,
            CHILD_REPLY_OUT,
        ));
    }

    // 4. fail_out — bridge_out to $params.parent_net_id / $params.failure_place
    //    (the spawn handler injects both). No producer unless the child
    //    template explicitly routes a failure here; an unwired bridge_out is
    //    inert, so this is safe for v1.
    let mut fail = boundary_place(CHILD_FAIL_OUT, "Fail", "bridge_out");
    fail.bridge_out = Some(BridgeTargetDto {
        target_net_id: "$params.parent_net_id".to_string(),
        target_place_name: "$params.failure_place".to_string(),
        reply_to: None,
        reply_channels: None,
        label: None,
    });
    child.places.push(fail);

    Ok(())
}

/// Derive a SubWorkflow node's **fixed** input/output port contract from the
/// referenced child template's high-level [`WorkflowGraph`].
///
/// - **input** = the child's `Start { initial }` port — its user-declared
///   input contract (the same shape an agent-tool `input_schema` is built
///   from). Absent Start ⇒ empty (permissive) input.
/// - **output** = the union (dedup by `name`, first-seen order) of every
///   `End { result_mapping }` entry's `target_field`, each typed [`FieldKind::Json`].
///   This is exactly what the child returns as `exit_code.value` and what the
///   parent's join unwraps (see `lower/subworkflow.rs`). No result mapping
///   anywhere ⇒ empty fields = opaque pass-through (pre-derivation behavior).
///
/// This is the **single** derivation shared by the publish path
/// (`resolve_subworkflow_air`) and the editor's `io-contract` endpoint, so the
/// authoring preview can never drift from the contract frozen at publish.
pub fn derive_child_io(child_graph: &WorkflowGraph) -> (Port, Port) {
    let input = child_graph
        .nodes
        .iter()
        .find_map(|n| match &n.data {
            WorkflowNodeData::Start { initial, .. } => Some(initial.clone()),
            _ => None,
        })
        .unwrap_or_else(Port::empty_input);

    let mut fields: Vec<PortField> = Vec::new();
    for node in &child_graph.nodes {
        let WorkflowNodeData::End { result_mapping, .. } = &node.data else {
            continue;
        };
        for m in result_mapping {
            let name = m.target_field.trim();
            if name.is_empty() || fields.iter().any(|f| f.name == name) {
                continue;
            }
            fields.push(PortField {
                schema: None,
                name: name.to_string(),
                label: name.to_string(),
                kind: FieldKind::Json,
                required: false,
                options: None,
                description: None,
                accept: None,
            });
        }
    }

    let output = Port {
        id: "out".to_string(),
        label: "Result".to_string(),
        fields,
    };

    (input, output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(id: &str, ty: &str) -> ScenarioPlace {
        boundary_place(id, id, ty)
    }

    fn minimal_child() -> ScenarioDefinition {
        ScenarioDefinition {
            name: "child".to_string(),
            description: None,
            places: vec![
                place("p_s_ready", "state"),
                place("p_e_done", "terminal"),
            ],
            transitions: vec![forward_transition(
                "t_s_park",
                "park",
                "p_s_ready",
                "p_e_done",
            )],
            groups: vec![],
            mock_adapters: vec![],
            definitions: Default::default(),
            requirements: vec![],
        }
    }

    #[test]
    fn adds_fixed_boundary_places() {
        let mut c = minimal_child();
        make_child_callable(&mut c, "p_s_ready", &["p_e_done".to_string()]).unwrap();

        let inbox = c.places.iter().find(|p| p.id == CHILD_INBOX).unwrap();
        assert_eq!(inbox.place_type, "bridge_in");

        let reply = c.places.iter().find(|p| p.id == CHILD_REPLY_OUT).unwrap();
        assert_eq!(reply.place_type, "state");
        assert!(reply.bridge_reply, "reply_out must be a bridge_reply place");

        let fail = c.places.iter().find(|p| p.id == CHILD_FAIL_OUT).unwrap();
        assert_eq!(fail.place_type, "bridge_out");
        let bo = fail.bridge_out.as_ref().unwrap();
        assert_eq!(bo.target_net_id, "$params.parent_net_id");
        assert_eq!(bo.target_place_name, "$params.failure_place");
    }

    #[test]
    fn flips_terminal_and_wires_connectors() {
        let mut c = minimal_child();
        make_child_callable(&mut c, "p_s_ready", &["p_e_done".to_string()]).unwrap();

        // Former terminal is now plain state (no NetCompleted race).
        let t = c.places.iter().find(|p| p.id == "p_e_done").unwrap();
        assert_eq!(t.place_type, "state");

        // inbox → Start entry connector.
        let inbox_t = c.transitions.iter().find(|t| t.id == "__sub_inbox").unwrap();
        assert_eq!(inbox_t.inputs[0].place, CHILD_INBOX);
        assert_eq!(inbox_t.outputs[0].place, "p_s_ready");

        // terminal → reply_out connector.
        let reply_t = c
            .transitions
            .iter()
            .find(|t| t.id == "__sub_reply_0")
            .unwrap();
        assert_eq!(reply_t.inputs[0].place, "p_e_done");
        assert_eq!(reply_t.outputs[0].place, CHILD_REPLY_OUT);
    }

    #[test]
    fn rejects_child_without_terminal() {
        let mut c = minimal_child();
        let err = make_child_callable(&mut c, "p_s_ready", &[]).unwrap_err();
        assert!(matches!(err, CompileError::Compilation(_)));
    }

    #[test]
    fn rejects_missing_entry_place() {
        let mut c = minimal_child();
        let err =
            make_child_callable(&mut c, "p_nonexistent", &["p_e_done".to_string()]).unwrap_err();
        assert!(matches!(err, CompileError::Compilation(_)));
    }

    #[test]
    fn rejects_double_application() {
        let mut c = minimal_child();
        make_child_callable(&mut c, "p_s_ready", &["p_e_done".to_string()]).unwrap();
        // Second application collides on `inbox`.
        let err =
            make_child_callable(&mut c, "p_s_ready", &["p_e_done".to_string()]).unwrap_err();
        assert!(matches!(err, CompileError::Compilation(_)));
    }

    // -------------------------------------------------------------------
    // derive_child_io
    // -------------------------------------------------------------------

    use crate::models::template::{
        FieldMapping, Position, WorkflowEdge, WorkflowNode,
    };

    fn gnode(id: &str, data: WorkflowNodeData) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: String::new(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data,
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn graph(nodes: Vec<WorkflowNode>) -> WorkflowGraph {
        WorkflowGraph {
            nodes,
            edges: Vec::<WorkflowEdge>::new(),
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(), default_scheduler: None,
        }
    }

    fn field(name: &str, kind: FieldKind) -> PortField {
        PortField {
            schema: None,
            name: name.to_string(),
            label: name.to_string(),
            kind,
            required: false,
            options: None,
            description: None,
            accept: None,
        }
    }

    fn rm(target: &str, expr: &str) -> FieldMapping {
        FieldMapping {
            target_field: target.to_string(),
            expression: expr.to_string(),
        }
    }

    fn start(initial: Port) -> WorkflowNodeData {
        WorkflowNodeData::Start {
            label: "Start".to_string(),
            description: None,
            initial,
            process_name: None,
        }
    }

    fn end(result_mapping: Vec<FieldMapping>) -> WorkflowNodeData {
        WorkflowNodeData::End {
            label: "End".to_string(),
            description: None,
            terminal: Port::empty_input(),
            result_mapping,
        }
    }

    #[test]
    fn derives_input_from_start_and_output_from_end_mappings() {
        let initial = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![field("message", FieldKind::Text), field("amount", FieldKind::Number)],
        };
        let g = graph(vec![
            gnode("s", start(initial.clone())),
            gnode(
                "e",
                end(vec![rm("invoice_amount", "review.amount"), rm("status", "decision.outcome")]),
            ),
        ]);

        let (input, output) = derive_child_io(&g);

        // Input is the Start initial port verbatim (typed).
        assert_eq!(input.fields.len(), 2);
        assert_eq!(input.fields[0].name, "message");
        assert_eq!(input.fields[1].kind, FieldKind::Number);

        // Output is the End result_mapping targets, Json-typed, in order.
        let names: Vec<&str> = output.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["invoice_amount", "status"]);
        assert!(output.fields.iter().all(|f| f.kind == FieldKind::Json));
    }

    #[test]
    fn unions_and_dedups_across_multiple_ends() {
        let g = graph(vec![
            gnode("s", start(Port::empty_input())),
            gnode("e1", end(vec![rm("a", "x.a"), rm("b", "x.b")])),
            gnode("e2", end(vec![rm("b", "y.b"), rm("c", "y.c"), rm("  ", "ignored")])),
        ]);

        let (_input, output) = derive_child_io(&g);
        let names: Vec<&str> = output.fields.iter().map(|f| f.name.as_str()).collect();
        // First-seen order, dedup `b`, blank target skipped.
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn empty_result_mapping_is_passthrough() {
        let g = graph(vec![
            gnode("s", start(Port::empty_input())),
            gnode("e", end(vec![])),
        ]);
        let (_input, output) = derive_child_io(&g);
        assert!(output.fields.is_empty(), "no mapping ⇒ opaque pass-through");
    }

    #[test]
    fn missing_start_yields_empty_input() {
        let g = graph(vec![gnode("e", end(vec![rm("a", "x.a")]))]);
        let (input, output) = derive_child_io(&g);
        assert!(input.fields.is_empty());
        assert_eq!(output.fields.len(), 1);
    }
}
