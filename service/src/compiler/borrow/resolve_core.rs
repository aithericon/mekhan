//! Single source for the producer-borrow **location invariants**.
//!
//! Both reference resolvers — `guard::resolve_ref` (Rhai-source guard refs)
//! and `automated_step::resolve_backend_ref` / the Envelope planner arm
//! (`{{slug.attr}}` placeholder refs) — enforce the same four invariants
//! before a borrow is admitted:
//!
//! 1. the head names a known producer slug ([`producer_for_slug`]),
//! 2. the producer is not the consumer itself (no self-reference),
//! 3. the producer is upstream of the consumer (per an [`UpstreamRule`]),
//! 4. the producer parks its output (`is_parked_producer`).
//!
//! [`check_borrowable_producer`] runs 2–4 in exactly that order. The
//! resolvers stay thin adapters around this core because the parts that
//! legitimately differ stay at each site: **field resolution** is
//! shape-context-specific (guards resolve against the full `TokenShape`
//! model — control-token discrimination, Map `[*]` boundaries, opaque-prefix
//! lease namespaces — while backend refs resolve flat `(slug, attr)` against
//! the producer's declared port), and **failure mapping** differs (guards
//! degrade to `RefResolution::Unresolved` → `GuardUnresolved` /
//! `UnresolvedGuardPath`; PerField backends hard-error with
//! `BackendRefUnresolved` / `BackendRefNotUpstream`; Envelope backends
//! silently skip).

use std::collections::BTreeMap;

use crate::compiler::token_shape::{is_loop_node, is_parked_producer, SlugIndex};
use crate::models::template::{WorkflowGraph, WorkflowNode, WorkflowNodeData};

/// Which location invariant a candidate borrow violates. Every site maps
/// these onto its own failure surface (see module docs) — the variants exist
/// so tests can pin *which* invariant fired and in what order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvariantViolation {
    /// The head names no producer slug (invariant 1).
    UnknownSlug,
    /// The producer is the consumer itself (invariant 2).
    SelfRef,
    /// The producer is not upstream of the consumer under the requested
    /// [`UpstreamRule`] (invariant 3).
    NotUpstream,
    /// The producer does not park its output (invariant 4).
    NotParked,
}

/// How invariant 3 ("the producer is upstream") is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpstreamRule {
    /// Plain topological position compare — `pos[producer] < pos[consumer]`,
    /// with the historical defaults (`usize::MAX` for an unknown producer,
    /// `0` for an unknown consumer, so a missing position fails closed).
    /// Used by backend refs (`resolve_backend_ref`) and the Envelope arm.
    StrictTopo,
    /// [`StrictTopo`](Self::StrictTopo) **or** the LeaseScope-containment
    /// recovery ([`producer_upstream_of`]) **or** the Loop body-child
    /// exception: the consumer is a Loop node and the producer is one of its
    /// direct body children (`producer.parent_id == consumer_id`). At
    /// continue-time the body has already parked its output, so a Loop
    /// accumulator `merge_expr` / `loop_condition` borrowing
    /// `<body_slug>.<field>` is sound even though the body sits downstream
    /// of the loop in topo order. Used by guard refs (`resolve_ref`).
    GuardReachability,
}

/// Invariant 1: resolve a reference head to its producer node id via the
/// same [`SlugIndex`] lookup every resolver uses.
pub(crate) fn producer_for_slug(
    slug: &str,
    slugs: &SlugIndex,
) -> Result<String, InvariantViolation> {
    slugs
        .node_for(slug)
        .map(str::to_string)
        .ok_or(InvariantViolation::UnknownSlug)
}

/// Invariants 2–4 on an already-resolved producer, in this order:
/// self-reference → upstream (per `rule`) → parked producer. The order is
/// load-bearing: a downstream non-parked producer reports `NotUpstream`,
/// not `NotParked` (matching what each adapter's error mapping historically
/// surfaced).
pub(crate) fn check_borrowable_producer(
    prod_id: &str,
    consumer_id: &str,
    graph: &WorkflowGraph,
    pos: &BTreeMap<String, usize>,
    rule: UpstreamRule,
) -> Result<(), InvariantViolation> {
    if prod_id == consumer_id {
        return Err(InvariantViolation::SelfRef);
    }
    let upstream = match rule {
        UpstreamRule::StrictTopo => {
            let up = pos.get(prod_id).copied().unwrap_or(usize::MAX);
            let me = pos.get(consumer_id).copied().unwrap_or(0);
            up < me
        }
        UpstreamRule::GuardReachability => {
            // EXCEPTION: a Loop accumulator's `merge_expr` (emitted into the
            // loop's `t_<id>_continue` logic) borrows the CURRENT iteration's
            // body output (`<body_slug>.<field>`). The body is the loop's
            // child (`parent_id == loop.id`), so it sits *downstream* of the
            // loop in topo order — the strict-upstream check would reject it.
            // But at continue-time the body has already produced its parked
            // output, so the read-arc into `p_<body>_data` is sound. Allow
            // the borrow when the consumer is a Loop and the producer is one
            // of its body children. (Only reachable via the accumulator scan;
            // loop_condition borrows of body output were already valid for
            // the same reason and simply weren't expressible before.)
            let producer_is_body_child = is_loop_node(graph, consumer_id)
                && graph
                    .nodes
                    .iter()
                    .find(|n| n.id == prod_id)
                    .and_then(|n| n.parent_id.as_deref())
                    == Some(consumer_id);
            producer_is_body_child
                || graph
                    .nodes
                    .iter()
                    .find(|n| n.id == consumer_id)
                    .map(|consumer| producer_upstream_of(prod_id, consumer, graph, pos))
                    .unwrap_or(false)
        }
    };
    if !upstream {
        return Err(InvariantViolation::NotUpstream);
    }
    if !is_parked_producer(graph, prod_id) {
        return Err(InvariantViolation::NotParked);
    }
    Ok(())
}

/// Walk `node_id`'s `parent_id` chain, collecting the ids of every enclosing
/// `LeaseScope` (innermost first).
pub(crate) fn enclosing_lease_scopes(node_id: &str, graph: &WorkflowGraph) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = graph
        .nodes
        .iter()
        .find(|n| n.id == node_id)
        .and_then(|n| n.parent_id.as_deref());
    while let Some(pid) = cur {
        let Some(p) = graph.nodes.iter().find(|n| n.id == pid) else {
            break;
        };
        if matches!(p.data, WorkflowNodeData::LeaseScope { .. }) {
            out.push(pid.to_string());
        }
        cur = p.parent_id.as_deref();
    }
    out
}

/// True if `node_id` is `ancestor_id` itself, or nested within it via the
/// `parent_id` chain.
pub(crate) fn is_within(node_id: &str, ancestor_id: &str, graph: &WorkflowGraph) -> bool {
    if node_id == ancestor_id {
        return true;
    }
    let mut cur = graph
        .nodes
        .iter()
        .find(|n| n.id == node_id)
        .and_then(|n| n.parent_id.as_deref());
    while let Some(pid) = cur {
        if pid == ancestor_id {
            return true;
        }
        cur = graph
            .nodes
            .iter()
            .find(|n| n.id == pid)
            .and_then(|n| n.parent_id.as_deref());
    }
    false
}

/// Borrow-ordering predicate: is `producer` strictly upstream of `consumer`,
/// such that a read-arc into the producer's parked `p_<producer>_data` is sound?
///
/// Primary rule: strict topological position (`pos[producer] < pos[consumer]`).
///
/// **LeaseScope recovery.** The topo DAG drops a scope's `body_out` return arc
/// (it would close the cycle `scope → body_in → … → body_out → scope`), so the
/// scope collapses to ONE node whose straight-through `out` successor can sort
/// BEFORE the body branch — even though at runtime `t_<scope>_exit` consumes the
/// body's *final* token (every body producer has parked its output) before
/// forwarding to the post-scope continuation. Recover the true ordering: a
/// producer contained in a LeaseScope `S` is upstream of any consumer that is
/// OUTSIDE `S` (not nested within it) and at-or-after `S` in topo order (the
/// scope node is a real DAG predecessor of its post-exit successors, so
/// `pos[S] < pos[consumer]`). A consumer *inside* the same scope falls back to
/// the strict topo check (a body node can't borrow a later sibling's gathered
/// output).
pub(crate) fn producer_upstream_of(
    producer: &str,
    consumer: &WorkflowNode,
    graph: &WorkflowGraph,
    pos: &BTreeMap<String, usize>,
) -> bool {
    let up = pos.get(producer).copied().unwrap_or(usize::MAX);
    let me = pos.get(&consumer.id).copied().unwrap_or(0);
    if up < me {
        return true;
    }
    for scope_id in enclosing_lease_scopes(producer, graph) {
        if is_within(&consumer.id, &scope_id, graph) {
            continue;
        }
        if pos.get(&scope_id).copied().unwrap_or(usize::MAX) < me {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::borrow::ctx::BorrowContext;
    use crate::compiler::token_shape::slug_index;
    use UpstreamRule::{GuardReachability, StrictTopo};

    /// Start → reviewA (parked human task, slug `rev_a`) → reviewB (parked,
    /// slug `rev_b`) → dec (Decision — NOT a parked producer) → end1/end2.
    fn linear_graph() -> WorkflowGraph {
        let step = r#"{"id":"s","title":"S","blocks":[{"type":"input","field":{"name":"amount","label":"Amt","kind":"number","required":true}}]}"#;
        let ht = |id: &str, slug: &str| {
            format!(
                r#"{{"id":"{id}","type":"human_task","slug":"{slug}","position":{{"x":0,"y":0}},"data":{{"type":"human_task","label":"{id}","taskTitle":"{id}","steps":[{step}]}}}}"#
            )
        };
        let json = format!(
            r#"{{"nodes":[
              {{"id":"start","type":"start","position":{{"x":0,"y":0}},"data":{{"type":"start","label":"Start"}}}},
              {ha},
              {hb},
              {{"id":"dec","type":"decision","slug":"dec","position":{{"x":0,"y":0}},"data":{{"type":"decision","label":"D","conditions":[{{"edgeId":"hi","label":"hi","guard":"rev_a.amount > 0"}}],"defaultBranch":"default"}}}},
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
        serde_json::from_str(&json).expect("deser linear graph")
    }

    #[test]
    fn producer_for_slug_resolves_and_flags_unknown() {
        let g = linear_graph();
        let slugs = slug_index(&g).expect("slug index");
        assert_eq!(producer_for_slug("rev_a", &slugs).as_deref(), Ok("reviewA"));
        assert_eq!(
            producer_for_slug("nope", &slugs),
            Err(InvariantViolation::UnknownSlug)
        );
    }

    #[test]
    fn self_ref_rejected_under_both_rules() {
        let g = linear_graph();
        let ctx = BorrowContext::build(&g).expect("ctx");
        for rule in [StrictTopo, GuardReachability] {
            assert_eq!(
                check_borrowable_producer("reviewA", "reviewA", &g, &ctx.pos, rule),
                Err(InvariantViolation::SelfRef),
                "{rule:?}"
            );
        }
    }

    #[test]
    fn downstream_producer_rejected_under_both_rules() {
        let g = linear_graph();
        let ctx = BorrowContext::build(&g).expect("ctx");
        for rule in [StrictTopo, GuardReachability] {
            assert_eq!(
                check_borrowable_producer("reviewB", "reviewA", &g, &ctx.pos, rule),
                Err(InvariantViolation::NotUpstream),
                "{rule:?}"
            );
        }
    }

    #[test]
    fn upstream_non_parked_producer_rejected_under_both_rules() {
        let g = linear_graph();
        let ctx = BorrowContext::build(&g).expect("ctx");
        // `dec` (Decision) is upstream of `end1` but parks nothing.
        for rule in [StrictTopo, GuardReachability] {
            assert_eq!(
                check_borrowable_producer("dec", "end1", &g, &ctx.pos, rule),
                Err(InvariantViolation::NotParked),
                "{rule:?}"
            );
        }
    }

    #[test]
    fn upstream_parked_producer_admitted_under_both_rules() {
        let g = linear_graph();
        let ctx = BorrowContext::build(&g).expect("ctx");
        for rule in [StrictTopo, GuardReachability] {
            assert_eq!(
                check_borrowable_producer("reviewA", "dec", &g, &ctx.pos, rule),
                Ok(()),
                "{rule:?}"
            );
        }
    }

    /// ASYMMETRY PIN (a): a producer inside a LeaseScope, borrowed by a
    /// consumer AFTER the scope. The topo DAG drops the scope's `body_out`
    /// return arc, so the post-scope consumer can sort before the body —
    /// StrictTopo rejects, GuardReachability recovers via the
    /// LeaseScope-containment rule. Mirrors
    /// `map_in_lease_scope_is_borrowable_after_the_scope`
    /// (token_shape/tests.rs).
    #[test]
    fn lease_scope_recovery_is_guard_only() {
        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start",
                "initial":{"id":"in","label":"Intake","fields":[
                  {"name":"rows","label":"Rows","kind":"json","required":true}]}}},
            {"id":"lease","type":"lease_scope","slug":"cell","position":{"x":120,"y":0},
             "data":{"type":"lease_scope","label":"Cell","lease":{"pool":"xarm_fleet"}}},
            {"id":"mp","type":"map","slug":"work","parentId":"lease","position":{"x":40,"y":60},
             "data":{"type":"map","label":"Per row","itemsRef":"start.rows","itemVar":"row","resultVar":"done",
                "output":{"id":"out","label":"Done","fields":[
                  {"name":"done","label":"Done","kind":"bool","required":true}]}}},
            {"id":"step","type":"automated_step","slug":"step","parentId":"mp","position":{"x":40,"y":60},
             "data":{"type":"automated_step","label":"Do",
                "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                "deploymentModel":{"mode":"executor"},
                "output":{"id":"out","label":"Done","fields":[
                  {"name":"done","label":"Done","kind":"bool","required":true}]}}},
            {"id":"end","type":"end","slug":"end","position":{"x":420,"y":0},
             "data":{"type":"end","label":"End","resultMapping":[
               {"targetField":"summary","expression":"work[*].done"}]}}
          ],
          "edges":[
            {"id":"e0","source":"start","target":"lease","targetHandle":"in","type":"sequence"},
            {"id":"e1","source":"lease","sourceHandle":"body_in","target":"mp","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"mp","sourceHandle":"body_in","target":"step","targetHandle":"in","type":"sequence"},
            {"id":"e3","source":"step","target":"mp","targetHandle":"body_out","type":"loop_back"},
            {"id":"e4","source":"mp","target":"lease","targetHandle":"body_out","type":"sequence"},
            {"id":"e5","source":"lease","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser lease-map graph");
        let ctx = BorrowContext::build(&g).expect("ctx");
        assert_eq!(
            check_borrowable_producer("mp", "end", &g, &ctx.pos, StrictTopo),
            Err(InvariantViolation::NotUpstream),
            "strict topo must reject the lease-contained producer"
        );
        assert_eq!(
            check_borrowable_producer("mp", "end", &g, &ctx.pos, GuardReachability),
            Ok(()),
            "guard reachability must recover via LeaseScope containment"
        );
    }

    /// ASYMMETRY PIN (b): a Loop consumer borrowing its OWN body child's
    /// parked output (accumulator `merge_expr` / `loop_condition`). The body
    /// is downstream in topo order, so StrictTopo rejects; GuardReachability
    /// admits via the body-child exception.
    #[test]
    fn loop_body_child_exception_is_guard_only() {
        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"lp","type":"loop","slug":"lp","position":{"x":120,"y":0},
             "data":{"type":"loop","label":"Iterate","maxIterations":3,"loopCondition":"lp.iteration < 3"}},
            {"id":"tick","type":"automated_step","slug":"tick","parentId":"lp","position":{"x":40,"y":60},
             "data":{"type":"automated_step","label":"Tick",
                "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                "deploymentModel":{"mode":"executor"},
                "output":{"id":"out","label":"Out","fields":[
                  {"name":"n","label":"N","kind":"number","required":true}]}}},
            {"id":"end","type":"end","position":{"x":420,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e0","source":"start","target":"lp","targetHandle":"in","type":"sequence"},
            {"id":"e1","source":"lp","sourceHandle":"body_in","target":"tick","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"tick","target":"lp","targetHandle":"body_out","type":"loop_back"},
            {"id":"e3","source":"lp","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser loop graph");
        let ctx = BorrowContext::build(&g).expect("ctx");
        assert_eq!(
            check_borrowable_producer("tick", "lp", &g, &ctx.pos, StrictTopo),
            Err(InvariantViolation::NotUpstream),
            "strict topo must reject the body child"
        );
        assert_eq!(
            check_borrowable_producer("tick", "lp", &g, &ctx.pos, GuardReachability),
            Ok(()),
            "guard reachability must admit the Loop body-child exception"
        );
    }
}
