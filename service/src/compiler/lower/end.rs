//! `WorkflowNodeData::End` lowering. Optional `resultMapping` stamps an
//! `exit_code = { ok: true, value }` envelope (preserving any prior
//! Failure-stamped envelope), then completes the named process (if Start
//! registered one) before tagging the terminal place.

use super::*;

pub(super) fn lower_end(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::End {
        label,
        result_mapping,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_end on non-End node")
    };
    let (rv_lets, rv_val) = result_mapping_rhai(result_mapping);
    let has_result = !result_mapping.is_empty();
    let ctx = &mut *cx.ctx;

    // Incoming edges always land in `p_{id}_done` — keep that id
    // stable (edge wiring + pass-through merges key off the End's
    // input_place).
    let done_id = format!("p_{id}_done");
    let done: PlaceHandle<DynamicToken> = ctx.state(&done_id, label);

    // When (and only when) a result is declared, insert a pure-Rhai shape
    // transition between the stable `p_{id}_done` and the terminal feed that
    // stamps the success envelope onto `exit_code`. The `if "exit_code" in`
    // guard is the Failure→End precedence rule: a token that already flowed
    // through a Failure node keeps its `{ ok: false }` envelope; only an
    // otherwise-unstamped token gets `{ ok: true, value }`. Skipped entirely
    // for bare End so the terminal token (and the instance `result`) is
    // byte-identical to pre-feature behavior.
    let (terminal_feed, terminal_feed_id) = if has_result {
        let shaped: PlaceHandle<DynamicToken> =
            ctx.state(format!("p_{id}_result"), format!("{label} - Result"));
        let logic = format!(
            "{rv_lets}let __rv = {rv_val}; let __out = input; \
             if \"exit_code\" in __out {{ }} \
             else {{ __out.exit_code = #{{ ok: true, value: __rv }}; }} \
             #{{ result: __out }}"
        );
        ctx.transition(
            format!("t_{id}_result_shape"),
            format!("{label} - Result"),
        )
        .auto_input("input", &done)
        .auto_output("result", &shaped)
        .logic_rhai(with_pluck_prelude(&logic))
        .done();
        (shaped, format!("p_{id}_result"))
    } else {
        (done.clone(), done_id)
    };

    let terminal_id = match cx.fixups.process_token_place.clone() {
        // No process was registered by the Start (opt-in unused).
        // Mint an End-owned terminal place plus a forwarding transition
        // so the workflow exit is anchored on a place this End emitted —
        // not on the upstream's `_ctrl` survivor of the pass-through
        // edge merge. Without this step, an upstream parking node
        // (Agent, AutomatedStep, HumanTask, …) whose `p_<upstream>_ctrl`
        // is the merge survivor of `p_{id}_done` ends up tagged
        // `terminal`; the engine then declares the net complete the
        // instant the upstream yields its slim control token, before
        // any End-side projection runs. Symptom: instance status flips
        // to `completed` with an empty/`{status: succeeded}` result and
        // the End node stays `pending` in the UI because no End-tagged
        // transition ever fires.
        None => {
            let exit: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_terminal"), format!("{label} - Exit"));
            ctx.transition(
                format!("t_{id}_complete"),
                format!("{label} - Complete"),
            )
            .auto_input("input", &terminal_feed)
            .auto_output("output", &exit)
            .logic_rhai("#{ output: input }".to_string())
            .done();
            let _ = terminal_feed_id;
            format!("p_{id}_terminal")
        }
        // A Start registered a process — mirror the Start pattern:
        // insert a `process_complete` effect between the (post-shape)
        // feed place and a new terminal. The handler reads `process_id`
        // from the parked `ProcessStarted` token via a read-arc
        // (non-consuming, so multiple End nodes each complete), passes
        // the workflow token through unchanged (so any stamped
        // `exit_code` survives), and the causality projector picks up
        // `completed: true`.
        Some(proc_place) => {
            let completed: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_completed"), format!("{label} - Completed"));
            ctx.transition(
                format!("t_{id}_proc_complete"),
                format!("{label} - Complete Process"),
            )
            .read_input("process", &proc_place)
            .auto_input("done", &terminal_feed)
            .auto_output("completed", &completed)
            .process_complete();

            format!("p_{id}_completed")
        }
    };

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: done,
            output_places: vec![],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    // Protocol: publish (derives entry from ports) then enrich with the
    // workflow-exit terminal. Recorded pre-alias-collapse; `compile.rs`
    // rewrites every interface place id through the alias map post-merge.
    cx.publish_interface().workflow_terminals.push(terminal_id);
    Ok(())
}
