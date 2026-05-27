//! `WorkflowNodeData::Decision` lowering. One transition per branch with a
//! switch/case-style fallthrough guard (`(g_i) && !g_{i-1} && … && !g_0`) so
//! at most one branch is enabled per token regardless of the engine's
//! enabling-time / specificity / id tiebreak. Optional `default_branch` +
//! observable dead-end-on-no-match.

use super::*;

pub(super) fn lower_decision(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Decision {
        label,
        conditions,
        default_branch,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_decision on non-Decision node")
    };
    let ctx = &mut *cx.ctx;

    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));

    let mut output_places = Vec::new();

    // Normalize once: a blank guard means "always match".
    let guards: Vec<String> = conditions
        .iter()
        .map(|c| {
            if c.guard.trim().is_empty() {
                "true".to_string()
            } else {
                c.guard.clone()
            }
        })
        .collect();
    let n = guards.len();

    // One transition per condition. Precedence is declaration order: branch i
    // fires only when its own guard holds AND every higher-precedence guard
    // does not (switch/case fallthrough). This keeps at most one branch (or
    // the default) enabled per token, so ordering is structural and does not
    // hinge on the engine's enabling-time / specificity / id tiebreak — which
    // can otherwise be skewed by read-arcs injected for borrowed guard data.
    // The descending priority is a redundant, declarative encoding of the same
    // order (and keeps default below every branch, dead-end below default).
    for (i, cond) in conditions.iter().enumerate() {
        let p_out: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_out_{i}"),
            format!("{label} - {}", cond.label),
        );

        let guard = if i == 0 {
            format!("({})", guards[0])
        } else {
            let exclude = (0..i)
                .rev()
                .map(|j| format!("!({})", guards[j]))
                .collect::<Vec<_>>()
                .join(" && ");
            format!("({}) && {exclude}", guards[i])
        };

        ctx.transition(
            format!("t_{id}_branch_{i}"),
            format!("{label} - {}", cond.label),
        )
        .auto_input("input", &p_input)
        .auto_output("output", &p_out)
        .guard_rhai(guard)
        .priority(format!("{}", n - i + 1))
        .logic_rhai("#{ output: input }")
        .done();

        output_places.push((Some(cond.edge_id.clone()), p_out));
    }

    // Default branch: the cascade's terminal `else` — enabled only when no
    // branch guard matched. With zero conditions it stays unconditional
    // (preserves the historical always-route behavior).
    if let Some(default_edge_id) = default_branch {
        let p_default: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_out_default"),
            format!("{label} - Default"),
        );

        let t = ctx
            .transition(format!("t_{id}_default"), format!("{label} - Default"))
            .auto_input("input", &p_input)
            .auto_output("output", &p_default)
            .priority("1");
        let t = if n > 0 {
            let none_match = (0..n)
                .map(|j| format!("!({})", guards[j]))
                .collect::<Vec<_>>()
                .join(" && ");
            t.guard_rhai(none_match)
        } else {
            t
        };
        t.logic_rhai("#{ output: input }").done();

        output_places.push((Some(default_edge_id.clone()), p_default));
    }

    // Unroutable token (no branch matched and no default, or a guard threw at
    // runtime so its negation poisoned the cascade) -> explicit, observable
    // net error instead of a silently stranded token. Unguarded + lowest
    // priority so it only ever wins when nothing else is enabled. The `throw`
    // is a permanent ScriptError: the engine emits ErrorOccurred and consumes
    // the token (no infinite re-fire).
    let deadend_msg =
        format!("decision {label}: token matched no branch and no default branch");
    ctx.transition(
        format!("t_{id}_deadend"),
        format!("{label} - No Match (error)"),
    )
    .auto_input("input", &p_input)
    .priority("0")
    .logic_rhai(format!("throw \"{}\"", rhai_str_escape(&deadend_msg)))
    .done();

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places,
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface();
    Ok(())
}
