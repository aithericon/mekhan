//! Streaming-channel lowering (docs/25, Phase 1 — control + data plane).
//!
//! An `AutomatedStep` may declare statically-typed streaming [`Channel`]s. The
//! job emits dynamic tokens into them mid-execution (`emit`/`scatter` for
//! control, `open_output` for data, via the SDK), routed by the engine's
//! `control_emit` effect into a synthesized per-channel place. This module owns
//! that synthesis, shared by the inline and pooled AutomatedStep lowering paths
//! so both expose identical channel topology.
//!
//! For each channel of an AutomatedStep `{id}`:
//!   - synthesize one place `p_{id}_{name}` (sanitized). For `direction: Out`
//!     it is registered in `NodePorts.output_places` keyed by the channel name
//!     so a downstream edge wires off `sourceHandle == name`; for
//!     `direction: In` it is registered in `input_handles` so an upstream edge
//!     wires off `targetHandle == name`.
//!
//! For the OUT-direction channels we ALSO synthesize the ingestion seam:
//!   - one control inbox signal place `p_{id}_control_in` where the executor's
//!     `control_emit` event lands (the engine `ExecutorWatcher` routes it there
//!     via the job's `event_routes["control_emit"]`).
//!   - one `t_{id}_control_emit` transition draining that inbox carrying the
//!     `control_emit` engine effect, whose `effect_config.channel_routes` maps
//!     each channel name → its synthesized place id. The handler reads the
//!     emit's `channel` field and deposits the token onto the resolved place.
//!
//! Plane-specific behaviour of the deposit:
//!   - **Control / `Signal`** — a `signal` emission lands verbatim on
//!     `p_{id}_{name}`; a downstream edge fires once per emission.
//!   - **Control / `Scatter`** — `scatter_item` / `scatter_close` tokens land on
//!     an internal raw place; we split them (close → coordinator, item →
//!     projected result) and fold them through the shared
//!     `gather::emit_gather_barrier` into a single collection token, so the
//!     channel's edge-wired consumer sees one gathered array — exactly the Map
//!     gather contract. `max_fanout` is enforced loudly: a `scatter_close` whose
//!     `count` exceeds the cap throws (→ NetFailed), never silently dropping.
//!   - **Data** (docs/25 §2-4) — the OPEN control token (`kind: open`), carrying
//!     the out-of-band transport DESCRIPTOR, lands verbatim on `p_{id}_{name}`
//!     and flows to the edge-wired consumer EARLY (the moment `open_output` is
//!     called, mid-job — independent of producer completion). The matching CLOSE
//!     token only updates producer status; bulk bytes never enter the marking,
//!     so a data channel adds exactly one consumer-facing place and NO
//!     scatter/gather split. Like every OUT channel it shares the one
//!     `control_emit` seam, so its place is ALSO a deposit place / output arc of
//!     `t_{id}_control_emit`.

use super::*;
use crate::models::template::{Channel, ChannelDirection, ChannelPlane, ControlContract};

/// One synthesized channel place, ready to fold into the node's `NodePorts`.
pub(crate) struct ChannelPort {
    /// The declared channel name — the `sourceHandle`/`targetHandle` edges wire on.
    pub(crate) name: String,
    /// `Out` registers in `output_places`; `In` registers in `input_handles`.
    pub(crate) direction: ChannelDirection,
    /// The synthesized place edges attach to. For a `Scatter` OUT channel this
    /// is the GATHERED place (the counted-barrier output), so the consumer sees
    /// one collection token; for a `Signal` OUT channel it is the raw deposit
    /// place; for an IN channel it is the raw inbound place.
    pub(crate) place: PlaceHandle<DynamicToken>,
}

/// Result of lowering an AutomatedStep's channels.
pub(crate) struct LoweredChannels {
    /// Per-channel wiring ports the caller folds into `NodePorts`.
    pub(crate) ports: Vec<ChannelPort>,
}

/// Sanitize a channel name into the `p_{id}_{name}` place-id segment. Channel
/// names are validated (`validate_channels`) to be Rhai-ident-safe, but we keep
/// the place-id segment defensive — replace anything outside `[A-Za-z0-9_]`
/// with `_` so a stray name can never break the synthesized place id.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

/// Build the channel MANIFEST Rhai literal (`[#{ name, plane, contract,
/// element_kind }, …]`) baked into the executor job spec under `spec.channels`.
/// Matches `aithericon_executor_domain::ChannelManifestEntry` — the worker
/// validates each `emit`/`scatter` channel name against this manifest. Empty
/// (`[]`) when the node declares no channels, so the spec stays byte-stable for
/// channel-less steps.
pub(crate) fn channel_manifest_rhai(channels: &[Channel]) -> String {
    if channels.is_empty() {
        return "[]".to_string();
    }
    let entries: Vec<serde_json::Value> = channels
        .iter()
        .map(|c| {
            let plane = match c.plane {
                ChannelPlane::Control => "control",
                ChannelPlane::Data => "data",
            };
            let element_kind = match &c.element {
                crate::models::template::ElementType::Json { .. } => "json",
                crate::models::template::ElementType::Binary { .. } => "binary",
                crate::models::template::ElementType::Any => "any",
            };
            let mut entry = serde_json::json!({
                "name": c.name,
                "plane": plane,
                "element_kind": element_kind,
            });
            if let Some(contract) = &c.contract {
                let v = match contract {
                    ControlContract::Signal => "signal",
                    ControlContract::Scatter => "scatter",
                };
                entry["contract"] = serde_json::Value::String(v.to_string());
            }
            entry
        })
        .collect();
    json_to_rhai_literal(&serde_json::Value::Array(entries))
}

/// Does this node declare ≥1 OUT channel (control OR data)? If so it needs the
/// `control_emit` ingestion seam (and thus the submit transition must register
/// `event_routes["control_emit"]` → the inbox). Both planes ride the same seam:
/// a control channel deposits `signal`/`scatter_*` tokens, a data channel
/// deposits its `open`/`close` bracket tokens — all via `control_emit`. Mirrors
/// the `out_channels` filter in [`lower_channels`] so the two never drift.
pub(crate) fn has_out_channel(channels: &[Channel]) -> bool {
    channels.iter().any(|c| matches!(c.direction, ChannelDirection::Out))
}

/// Pre-create the `control_emit` inbox place when the node has ≥1 OUT channel
/// (control or data), so the executor lifecycle's submit transition can register
/// `event_routes["control_emit"]` → this place id BEFORE [`lower_channels`]
/// drains it. Returns `None` (no place synthesized → AIR byte-stable) for a node
/// with no OUT channel. The same handle is then threaded back into
/// [`lower_channels`] so the inbox is created exactly once.
pub(crate) fn control_inbox(
    ctx: &mut Context,
    id: &str,
    label: &str,
    channels: &[Channel],
) -> Option<PlaceHandle<DynamicToken>> {
    if !has_out_channel(channels) {
        return None;
    }
    Some(ctx.signal(
        format!("p_{id}_control_in"),
        format!("{label} - Control Emit Inbox"),
    ))
}

/// Synthesize the streaming channel topology for an AutomatedStep.
///
/// Builds the per-channel places, the `control_emit` ingestion seam (inbox +
/// effect transition with `channel_routes`), and — for `Scatter` channels — the
/// counted gather barrier + `max_fanout` guard. Returns the wiring ports the
/// caller folds into the node's `NodePorts`.
///
/// `channels` is the node's declared `Vec<Channel>` (both planes are acted on:
/// control channels carry `signal`/`scatter_*` tokens, data channels carry the
/// `open`/`close` bracket — all via the one `control_emit` seam, since the
/// descriptor that opens a data channel is itself a control emission). A node
/// with no channels produces no places/transitions — AIR stays byte-stable.
///
/// `control_in` is the pre-created inbox place from [`control_inbox`] (the
/// lifecycle's submit transition already registered its id as the
/// `control_emit` event route). It is `Some` exactly when the node has ≥1 OUT
/// channel; passing the handle in (rather than re-creating it here) keeps the
/// inbox a single place wired both as the watcher's deposit target and as this
/// fan-out transition's input.
pub(crate) fn lower_channels(
    ctx: &mut Context,
    id: &str,
    label: &str,
    channels: &[Channel],
    control_in: Option<PlaceHandle<DynamicToken>>,
    input_place: &PlaceHandle<DynamicToken>,
) -> LoweredChannels {
    let mut ports: Vec<ChannelPort> = Vec::new();

    if channels.is_empty() {
        return LoweredChannels { ports };
    }

    // All OUT channels (control and data) share one ingestion seam: a control
    // inbox the executor's `control_emit` event lands on, drained by a single
    // transition that re-routes each emit by channel name. A data channel's
    // `open` descriptor token rides the SAME seam — so it joins `out_channels`
    // and gets a deposit place / output arc just like a control channel. Build
    // the seam only if any OUT channel exists.
    let out_channels: Vec<&Channel> =
        channels.iter().filter(|c| matches!(c.direction, ChannelDirection::Out)).collect();

    // `channel_routes`: channel name → synthesized DEPOSIT place id. For a
    // Signal channel the deposit place IS the consumer-facing place; for a
    // Scatter channel the deposit place is an internal raw place we then split +
    // gather (the consumer-facing place is the gathered output).
    let mut channel_routes = serde_json::Map::new();
    // The deposit places the `control_emit` handler routes tokens into. Each
    // must be declared as an OUTPUT ARC of `t_{id}_control_emit` (port name ==
    // place id) — the engine validates the handler's returned token keys (place
    // ids) against the transition's output ports (`firing.rs`), so an undeclared
    // deposit place is rejected as `UnknownOutputPort`.
    let mut deposit_places: Vec<PlaceHandle<DynamicToken>> = Vec::new();

    for ch in &out_channels {
        let seg = sanitize(&ch.name);

        // A DATA channel deposits its OPEN descriptor token verbatim onto the
        // consumer-facing place — same single-place shape as a `Signal` control
        // channel (no scatter/gather split; bulk bytes stay out-of-band). The
        // `open` token flows to the edge-wired consumer EARLY (mid-job, when
        // `open_output` is called); the matching `close` token only updates
        // producer status. Both `open` and `close` ride the `control_emit` seam
        // and are routed to this place, so it is registered as a deposit place /
        // output arc exactly like a control deposit place.
        if matches!(ch.plane, ChannelPlane::Data) {
            let p_chan: PlaceHandle<DynamicToken> = ctx.signal(
                format!("p_{id}_{seg}"),
                format!("{label} - Data Channel '{}'", ch.name),
            );
            channel_routes
                .insert(ch.name.clone(), serde_json::Value::String(p_chan.id().to_string()));
            deposit_places.push(p_chan.clone());
            ports.push(ChannelPort {
                name: ch.name.clone(),
                direction: ChannelDirection::Out,
                place: p_chan,
            });
            continue;
        }

        let contract = ch.contract.clone().unwrap_or(ControlContract::Signal);
        match contract {
            ControlContract::Signal => {
                // The deposit place is the consumer-facing place: a `signal`
                // emission lands here verbatim (`{ kind: "signal", payload }`)
                // and a downstream edge fires once per emission.
                let p_chan: PlaceHandle<DynamicToken> = ctx.signal(
                    format!("p_{id}_{seg}"),
                    format!("{label} - Channel '{}'", ch.name),
                );
                channel_routes.insert(
                    ch.name.clone(),
                    serde_json::Value::String(p_chan.id().to_string()),
                );
                deposit_places.push(p_chan.clone());
                ports.push(ChannelPort {
                    name: ch.name.clone(),
                    direction: ChannelDirection::Out,
                    place: p_chan,
                });
            }
            ControlContract::Scatter => {
                // Raw deposit place: receives BOTH `scatter_item` and
                // `scatter_close` tokens from the `control_emit` handler.
                let p_raw: PlaceHandle<DynamicToken> = ctx.signal(
                    format!("p_{id}_{seg}_raw"),
                    format!("{label} - Channel '{}' (raw scatter)", ch.name),
                );
                channel_routes.insert(
                    ch.name.clone(),
                    serde_json::Value::String(p_raw.id().to_string()),
                );
                deposit_places.push(p_raw.clone());

                // Split + gather. `p_count` = the coordinator (from
                // scatter_close), `p_results` = projected items, `p_gathered` =
                // the consumer-facing collection token.
                let p_count: PlaceHandle<DynamicToken> = ctx.state(
                    format!("p_{id}_{seg}_count"),
                    format!("{label} - Channel '{}' Gather Coordinator", ch.name),
                );
                let p_results: PlaceHandle<DynamicToken> = ctx.state(
                    format!("p_{id}_{seg}_results"),
                    format!("{label} - Channel '{}' Items", ch.name),
                );
                let p_gathered: PlaceHandle<DynamicToken> = ctx.state(
                    format!("p_{id}_{seg}_gathered"),
                    format!("{label} - Channel '{}' Gathered", ch.name),
                );

                // `max_fanout` guard. A Scatter channel always carries a
                // positive `max_fanout` (enforced by `validate_channels`); a
                // `scatter_close` whose `count` exceeds it throws → NetFailed
                // (the instance fails loudly, never silent-dropping items). The
                // cap also bounds the worst-case fan-out the barrier waits on.
                let max_fanout = ch.max_fanout.unwrap_or(0);

                // t_{id}_{seg}_close — consume the scatter_close token, enforce
                // the cap, and emit the gather coordinator `#{ count: <n>,
                // __map_id }`. The correlate id is the emit's `__map_id`.
                let fanout_msg = format!(
                    "scatter channel '{}' on step '{label}' exceeded max_fanout ({max_fanout})",
                    ch.name
                );
                ctx.transition(
                    format!("t_{id}_{seg}_close"),
                    format!("{label} - Channel '{}' Close", ch.name),
                )
                .auto_input("close", &p_raw)
                .auto_output("count", &p_count)
                .guard_rhai(r#"close.kind == "scatter_close""#)
                .logic_rhai(format!(
                    "if close.count > {max_fanout} {{ throw \"{}\"; }} \
                     #{{ count: #{{ count: close.count, \"__map_id\": close.__map_id }} }}",
                    rhai_str_escape(&fanout_msg)
                ))
                .done();

                // t_{id}_{seg}_item — consume each scatter_item token, project
                // it to the gather's `#{ value, __map_idx, __map_id }` shape.
                //
                // Per-ITEM `max_fanout` guard: the close-count cap can be desynced
                // from the actual emit count (a producer emits more items than it
                // stamps in `scatter_count`). A 0-based `__map_idx` reaching the
                // cap means the item count has exceeded it, so throw at the first
                // offending item — the instance fails loudly (NetFailed via
                // ScriptError), never a silent over-fanout orphan. This is the
                // per-item sibling of the close-count cap on `t_{id}_{seg}_close`.
                let item_fanout_msg = format!(
                    "scatter channel '{}' on step '{label}' exceeded max_fanout ({max_fanout}) — item index out of bounds",
                    ch.name
                );
                ctx.transition(
                    format!("t_{id}_{seg}_item"),
                    format!("{label} - Channel '{}' Item", ch.name),
                )
                .auto_input("item", &p_raw)
                .auto_output("result", &p_results)
                .guard_rhai(r#"item.kind == "scatter_item""#)
                .logic_rhai(format!(
                    r#"if item.__map_idx >= {max_fanout} {{ throw "{}"; }} #{{ result: #{{ value: item.payload, "__map_idx": item.__map_idx, "__map_id": item.__map_id }} }}"#,
                    rhai_str_escape(&item_fanout_msg)
                ))
                .done();

                // Shared counted barrier: read the coordinator for `count` +
                // `__map_id`, gather exactly that many items correlated on
                // `__map_id`, sort by `__map_idx`, reduce to `#{ output: [..] }`.
                super::gather::emit_gather_barrier(
                    ctx,
                    &format!("{id}_{seg}"),
                    label,
                    &p_count,
                    &p_results,
                    &p_gathered,
                    "count.count",
                    "__map_id",
                );

                // The consumer-facing place is the gathered collection.
                ports.push(ChannelPort {
                    name: ch.name.clone(),
                    direction: ChannelDirection::Out,
                    place: p_gathered,
                });
            }
        }
    }

    // IN-direction channels (control OR data): a place an UPSTREAM node emits
    // into. For a CONTROL channel the upstream's token lands here directly; for a
    // DATA channel the upstream's OPEN descriptor token lands here and the
    // consumer's `stream(name)` reads it as the consumer job's input (then
    // connects to the descriptor's out-of-band transport). Register an inbound
    // place edges target by `targetHandle == name`.
    for ch in channels.iter().filter(|c| matches!(c.direction, ChannelDirection::In)) {
        // An IN channel aliases the node's MAIN input place: the upstream's OPEN
        // descriptor token (data) — or a future inbound control token — must both
        // TRIGGER the node's job (the submit transition consumes `input_place`)
        // AND be present in the job's input (where `stream(name)` reads the
        // transport subject from the descriptor). A separate inbound place would
        // receive the token but never start the job → the consumer hangs. So the
        // `targetHandle == name` edge routes straight into `input_place`.
        ports.push(ChannelPort {
            name: ch.name.clone(),
            direction: ChannelDirection::In,
            place: input_place.clone(),
        });
    }

    // The ingestion seam: only when there is at least one OUT channel to emit
    // into. The control inbox is the `control_in` place pre-created by
    // `control_inbox` (a Signal place the executor's `control_emit` event lands
    // on via the job's `event_routes["control_emit"]` — registered on the submit
    // transition BEFORE this runs). The `t_{id}_control_emit` transition drains
    // it, carrying the `control_emit` engine effect with the `channel_routes`
    // map. The handler reads the emit's `channel` field and deposits the token
    // onto the resolved place — so this ONE transition fans every channel's
    // emissions out to their places. The `out_channels`-non-empty branch and the
    // `control_in`-is-`Some` branch agree (both keyed on `has_out_channel`).
    if let Some(p_control_in) = control_in {
        debug_assert!(
            !out_channels.is_empty(),
            "control inbox synthesized but no OUT channel — control_inbox/lower_channels drift"
        );
        let mut t = ctx
            .transition(
                format!("t_{id}_control_emit"),
                format!("{label} - Control Emit"),
            )
            .auto_input("emit", &p_control_in);
        // Declare every deposit place as an output arc (port name == place id),
        // so the engine accepts the handler's dynamically-routed token. The
        // handler returns at most one token per fire (for the channel named on
        // the emit), so the other declared ports simply receive nothing.
        for place in &deposit_places {
            t = t.auto_output(place.id().to_string(), place);
        }
        t.effect_with_config(
            effects::CONTROL_EMIT.handler_id,
            serde_json::json!({ "channel_routes": serde_json::Value::Object(channel_routes) }),
        );
    }

    LoweredChannels { ports }
}

#[cfg(test)]
mod tests {
    use crate::compiler::{compile_to_air, validate::validate_channels};
    use crate::models::template::WorkflowGraph;
    use serde_json::json;

    /// A linear `start → step → end` graph where `step` is an AutomatedStep
    /// declaring the given `channels` array.
    fn graph_with_channels(channels: serde_json::Value) -> WorkflowGraph {
        serde_json::from_value(json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"step","type":"automated_step","slug":"step","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Step",
                     "executionSpec":{"backendType":"docker","config":{"image":"alpine:latest"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"executor"},
                     "channels": channels}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"step","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"step","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }))
        .expect("graph fixture")
    }

    fn signal_channel() -> serde_json::Value {
        json!({ "name": "events", "direction": "out", "plane": "control",
                "element": { "type": "any" }, "contract": "signal" })
    }

    fn scatter_channel() -> serde_json::Value {
        json!({ "name": "items", "direction": "out", "plane": "control",
                "element": { "type": "any" }, "contract": "scatter", "max_fanout": 16 })
    }

    fn data_channel() -> serde_json::Value {
        json!({ "name": "frames", "direction": "out", "plane": "data",
                "element": { "type": "binary", "content_type": "image/jpeg" } })
    }

    fn place_ids(air: &serde_json::Value) -> Vec<String> {
        air["places"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap().to_string())
            .collect()
    }

    fn transition<'a>(air: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
        air["transitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap_or_else(|| panic!("transition {id} not found"))
    }

    #[test]
    fn signal_channel_synthesizes_place_and_control_emit_route() {
        let graph = graph_with_channels(json!([signal_channel()]));
        let air =
            compile_to_air(&graph, "ch-signal", "d", &std::collections::HashMap::new()).unwrap();

        let places = place_ids(&air);
        assert!(
            places.iter().any(|p| p == "p_step_events"),
            "signal channel place missing: {places:?}"
        );
        assert!(
            places.iter().any(|p| p == "p_step_control_in"),
            "control inbox missing: {places:?}"
        );

        // The control_emit transition carries the channel_routes effect_config.
        let t = transition(&air, "t_step_control_emit");
        let logic = &t["logic"];
        let handler = logic
            .get("Effect")
            .and_then(|e| e.get("handler_id"))
            .or_else(|| logic.get("handler_id"))
            .and_then(|h| h.as_str())
            .expect("control_emit is an effect transition");
        assert_eq!(handler, "control_emit");
        let config = logic
            .get("Effect")
            .and_then(|e| e.get("config"))
            .or_else(|| logic.get("config"))
            .expect("config present");
        assert_eq!(
            config["channel_routes"]["events"], "p_step_events",
            "channel_routes must map the channel name to its place; got {config}"
        );
    }

    #[test]
    fn scatter_channel_synthesizes_gather_and_split() {
        let graph = graph_with_channels(json!([scatter_channel()]));
        let air =
            compile_to_air(&graph, "ch-scatter", "d", &std::collections::HashMap::new()).unwrap();

        let places = place_ids(&air);
        for expect in [
            "p_step_items_raw",
            "p_step_items_count",
            "p_step_items_results",
            "p_step_items_gathered",
        ] {
            assert!(
                places.iter().any(|p| p == expect),
                "scatter place '{expect}' missing: {places:?}"
            );
        }
        // The split + gather transitions exist.
        for expect in [
            "t_step_items_close",
            "t_step_items_item",
            "t_step_items_gather",
        ] {
            transition(&air, expect);
        }
        // The route map points the channel at its RAW deposit place (the close /
        // item split consumes from there, the gathered place is consumer-facing).
        let config = {
            let t = transition(&air, "t_step_control_emit");
            t["logic"]
                .get("Effect")
                .and_then(|e| e.get("config"))
                .or_else(|| t["logic"].get("config"))
                .cloned()
                .unwrap()
        };
        assert_eq!(config["channel_routes"]["items"], "p_step_items_raw");
    }

    /// `t_{id}_control_emit` MUST declare each channel's deposit place as an
    /// OUTPUT ARC (port name == place id). The engine validates the handler's
    /// returned token keys (place ids) against the transition's output ports
    /// (`firing.rs` → `UnknownOutputPort`); without the arc the live emit path
    /// NetFails with "Unknown output port 'p_..._raw' returned by script".
    /// Regression guard for the demo-17 live-checkpoint failure.
    #[test]
    fn control_emit_declares_deposit_place_as_output_arc() {
        for (label, chan, deposit) in [
            ("signal", signal_channel(), "p_step_events"),
            ("scatter", scatter_channel(), "p_step_items_raw"),
        ] {
            let graph = graph_with_channels(json!([chan]));
            let air = compile_to_air(&graph, "ch-arc", "d", &std::collections::HashMap::new())
                .unwrap_or_else(|e| panic!("{label} compile failed: {e:?}"));
            let t = transition(&air, "t_step_control_emit");
            let outs = t["outputs"].as_array().unwrap_or_else(|| {
                panic!("{label}: t_step_control_emit has no output arcs: {t}")
            });
            assert!(
                outs.iter().any(|a| a["place"] == deposit),
                "{label}: control_emit must declare an output arc to deposit place '{deposit}'; got {outs:?}"
            );
        }
    }

    /// The executor SUBMIT transition (`{id}/submit`) must register the
    /// `control_emit` event route → the synthesized control inbox, so the
    /// engine's `ExecutorWatcher` knows where to deposit a mid-execution
    /// `ControlEmitEvent`. Without this the whole control-emit path is dead.
    #[test]
    fn submit_registers_control_emit_event_route() {
        let graph = graph_with_channels(json!([signal_channel()]));
        let air =
            compile_to_air(&graph, "ch-route", "d", &std::collections::HashMap::new()).unwrap();

        let submit = transition(&air, "step/submit");
        let config = submit["logic"]
            .get("Effect")
            .and_then(|e| e.get("config"))
            .or_else(|| submit["logic"].get("config"))
            .expect("submit is an effect transition with config");
        assert_eq!(
            config["event_routes"]["control_emit"], "p_step_control_in",
            "submit must route control_emit to the control inbox; got {config}"
        );
    }

    /// A channel-less step must NOT register a `control_emit` route (AIR stays
    /// byte-stable for steps with no OUT control channel).
    #[test]
    fn submit_omits_control_emit_route_without_channels() {
        let graph = graph_with_channels(json!([]));
        let air =
            compile_to_air(&graph, "ch-noroute", "d", &std::collections::HashMap::new()).unwrap();

        let submit = transition(&air, "step/submit");
        let event_routes = submit["logic"]
            .get("Effect")
            .and_then(|e| e.get("config"))
            .or_else(|| submit["logic"].get("config"))
            .and_then(|c| c.get("event_routes"));
        if let Some(routes) = event_routes {
            assert!(
                routes.get("control_emit").is_none(),
                "channel-less step must not register a control_emit route; got {routes}"
            );
        }
    }

    /// The scatter ITEM transition carries a per-item `max_fanout` guard
    /// (`item.__map_idx >= {max_fanout}` → throw), the per-item sibling of the
    /// close-count cap, so an over-fanout fails the instance loudly at the first
    /// offending item rather than silently orphaning it.
    #[test]
    fn scatter_item_has_per_item_max_fanout_guard() {
        let graph = graph_with_channels(json!([scatter_channel()]));
        let air =
            compile_to_air(&graph, "ch-guard", "d", &std::collections::HashMap::new()).unwrap();

        let item = transition(&air, "t_step_items_item");
        let src = item["logic"]
            .get("Rhai")
            .and_then(|l| l.get("source"))
            .or_else(|| item["logic"].get("source"))
            .and_then(|s| s.as_str())
            .expect("item transition rhai source");
        // scatter_channel() declares max_fanout: 16.
        assert!(
            src.contains("item.__map_idx >= 16"),
            "item transition must guard against over-fanout; got {src}"
        );
        assert!(
            src.contains("throw"),
            "over-fanout must throw (NetFailed), not silently drop; got {src}"
        );
    }

    /// A DATA channel synthesizes the consumer-facing place `p_{id}_{name}` (no
    /// scatter/gather split) — the place the OPEN descriptor token lands on and a
    /// downstream edge wires off `sourceHandle == name`.
    #[test]
    fn data_channel_synthesizes_consumer_place() {
        let graph = graph_with_channels(json!([data_channel()]));
        let air =
            compile_to_air(&graph, "ch-data", "d", &std::collections::HashMap::new()).unwrap();

        let places = place_ids(&air);
        assert!(
            places.iter().any(|p| p == "p_step_frames"),
            "data channel place missing: {places:?}"
        );
        assert!(
            places.iter().any(|p| p == "p_step_control_in"),
            "data channel must build the control_emit inbox (open/close ride that seam): {places:?}"
        );
        // No scatter/gather split for a data channel.
        for absent in ["p_step_frames_raw", "p_step_frames_gathered"] {
            assert!(
                !places.iter().any(|p| p == absent),
                "data channel must not synthesize scatter place '{absent}': {places:?}"
            );
        }
        // The route map points the channel name at its consumer place (the OPEN
        // descriptor deposits there).
        let t = transition(&air, "t_step_control_emit");
        let config = t["logic"]
            .get("Effect")
            .and_then(|e| e.get("config"))
            .or_else(|| t["logic"].get("config"))
            .expect("config present");
        assert_eq!(config["channel_routes"]["frames"], "p_step_frames");
    }

    /// The OPEN descriptor token deposits into `p_{id}_{name}`, so that place MUST
    /// be declared an OUTPUT ARC of `t_{id}_control_emit` (port == place id) — the
    /// engine validates handler token keys against the transition's output ports
    /// (`firing.rs` → `UnknownOutputPort`). This is the exact bug that NetFailed
    /// demo-17 live; mirror it for the data plane.
    #[test]
    fn data_channel_open_place_is_control_emit_output_arc() {
        let graph = graph_with_channels(json!([data_channel()]));
        let air =
            compile_to_air(&graph, "ch-data-arc", "d", &std::collections::HashMap::new()).unwrap();
        let t = transition(&air, "t_step_control_emit");
        let outs = t["outputs"]
            .as_array()
            .unwrap_or_else(|| panic!("t_step_control_emit has no output arcs: {t}"));
        assert!(
            outs.iter().any(|a| a["place"] == "p_step_frames"),
            "control_emit must declare an output arc to the data deposit place 'p_step_frames'; got {outs:?}"
        );
    }

    /// A data channel's consumer place is registered in `NodePorts.output_places`
    /// keyed by the channel name, so a downstream edge wiring off
    /// `sourceHandle == name` resolves to it and the graph compiles.
    #[test]
    fn data_channel_place_is_output_place_for_consumer_edge() {
        // producer `step` (OUT data `frames`) → consumer `sink` (an
        // AutomatedStep) via an edge wired off the data channel's source handle.
        let graph: WorkflowGraph = serde_json::from_value(json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"step","type":"automated_step","slug":"step","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Producer",
                     "executionSpec":{"backendType":"docker","config":{"image":"alpine:latest"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"executor"},
                     "channels":[data_channel()]}},
            {"id":"sink","type":"automated_step","slug":"sink","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Consumer",
                     "executionSpec":{"backendType":"docker","config":{"image":"alpine:latest"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"executor"},
                     "channels":[{ "name": "frames", "direction": "in", "plane": "data",
                                   "element": { "type": "binary", "content_type": "image/jpeg" } }]}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"step","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"step","sourceHandle":"frames","target":"sink","targetHandle":"frames","type":"sequence"},
            {"id":"e3","source":"sink","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }))
        .expect("data-wired graph fixture");
        // Compiles: the producer's `frames` output_place resolves the
        // `sourceHandle == "frames"` edge. The consumer's single-incoming IN place
        // is a pure pass-through, so `wire.rs` MERGES it into the producer's place
        // (the survivor) — the consumer reads the OPEN descriptor directly off
        // `p_step_frames`. The merge is exactly what proves the channel place was
        // registered as the producer's `output_place`: an unregistered handle
        // would have failed `find_output_place` ("no output place for
        // source_handle 'frames'") instead of compiling.
        let air = compile_to_air(&graph, "ch-data-wire", "d", &std::collections::HashMap::new())
            .expect("data channel must be edge-wireable off its source handle");
        let places = place_ids(&air);
        assert!(
            places.iter().any(|p| p == "p_step_frames"),
            "producer data place (the merge survivor the consumer reads) missing: {places:?}"
        );
    }

    #[test]
    fn manifest_is_baked_into_job_spec() {
        let graph = graph_with_channels(json!([signal_channel(), scatter_channel()]));
        let air =
            compile_to_air(&graph, "ch-manifest", "d", &std::collections::HashMap::new()).unwrap();
        let prepare = transition(&air, "step/prepare");
        let src = prepare["logic"]
            .get("Rhai")
            .and_then(|l| l.get("source"))
            .or_else(|| prepare["logic"].get("source"))
            .and_then(|s| s.as_str())
            .expect("prepare rhai source");
        assert!(src.contains(r#""channels":"#), "manifest key missing: {src}");
        assert!(src.contains(r#""name": "events""#), "signal entry missing");
        assert!(
            src.contains(r#""contract": "scatter""#),
            "scatter contract missing"
        );
    }

    #[test]
    fn no_channels_omits_topology() {
        let graph = graph_with_channels(json!([]));
        let air =
            compile_to_air(&graph, "ch-none", "d", &std::collections::HashMap::new()).unwrap();
        assert!(
            !place_ids(&air).iter().any(|p| p == "p_step_control_in"),
            "channel-less step must not synthesize a control inbox"
        );
    }

    #[test]
    fn validate_rejects_duplicate_names() {
        let graph = graph_with_channels(json!([signal_channel(), signal_channel()]));
        let err = validate_channels(&graph).unwrap_err();
        assert_eq!(err.kind(), "channel_invalid");
    }

    #[test]
    fn validate_rejects_scatter_without_positive_max_fanout() {
        let graph = graph_with_channels(json!([
            { "name": "items", "direction": "out", "plane": "control",
              "element": { "type": "any" }, "contract": "scatter" }
        ]));
        let err = validate_channels(&graph).unwrap_err();
        assert_eq!(err.kind(), "channel_invalid");
    }

    #[test]
    fn validate_rejects_control_channel_without_contract() {
        let graph = graph_with_channels(json!([
            { "name": "events", "direction": "out", "plane": "control",
              "element": { "type": "any" } }
        ]));
        let err = validate_channels(&graph).unwrap_err();
        assert_eq!(err.kind(), "channel_invalid");
    }

    #[test]
    fn validate_rejects_unresolved_json_element_schema() {
        let graph = graph_with_channels(json!([
            { "name": "events", "direction": "out", "plane": "control",
              "element": { "type": "json", "schema": { "$ref": "#/definitions/Missing" } },
              "contract": "signal" }
        ]));
        let err = validate_channels(&graph).unwrap_err();
        assert_eq!(err.kind(), "channel_invalid");
    }

    #[test]
    fn validate_accepts_well_formed_channels() {
        let graph = graph_with_channels(json!([signal_channel(), scatter_channel()]));
        assert!(validate_channels(&graph).is_ok());
    }

    #[test]
    fn validate_accepts_well_formed_data_channel() {
        let graph = graph_with_channels(json!([data_channel()]));
        assert!(validate_channels(&graph).is_ok());
    }

    #[test]
    fn validate_rejects_data_channel_with_max_fanout() {
        let graph = graph_with_channels(json!([
            { "name": "frames", "direction": "out", "plane": "data",
              "element": { "type": "any" }, "max_fanout": 4 }
        ]));
        let err = validate_channels(&graph).unwrap_err();
        assert_eq!(err.kind(), "channel_invalid");
    }

    #[test]
    fn validate_rejects_data_channel_with_contract() {
        let graph = graph_with_channels(json!([
            { "name": "frames", "direction": "out", "plane": "data",
              "element": { "type": "any" }, "contract": "signal" }
        ]));
        let err = validate_channels(&graph).unwrap_err();
        assert_eq!(err.kind(), "channel_invalid");
    }

    #[test]
    fn validate_rejects_binary_data_channel_with_empty_content_type() {
        let graph = graph_with_channels(json!([
            { "name": "frames", "direction": "out", "plane": "data",
              "element": { "type": "binary", "content_type": "  " } }
        ]));
        let err = validate_channels(&graph).unwrap_err();
        assert_eq!(err.kind(), "channel_invalid");
    }

    /// A channel edge whose source is a DATA OUT channel but whose target is a
    /// CONTROL IN channel (or any plane mismatch) is rejected — the payloads are
    /// incompatible (bytes vs. a flowing token).
    #[test]
    fn validate_rejects_cross_plane_channel_edge() {
        let graph: WorkflowGraph = serde_json::from_value(json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"step","type":"automated_step","slug":"step","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Producer",
                     "executionSpec":{"backendType":"docker","config":{"image":"alpine:latest"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"executor"},
                     "channels":[data_channel()]}},
            {"id":"sink","type":"automated_step","slug":"sink","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Consumer",
                     "executionSpec":{"backendType":"docker","config":{"image":"alpine:latest"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"executor"},
                     "channels":[{ "name": "frames", "direction": "in", "plane": "control",
                                   "element": { "type": "any" }, "contract": "signal" }]}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"step","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"step","sourceHandle":"frames","target":"sink","targetHandle":"frames","type":"sequence"},
            {"id":"e3","source":"sink","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }))
        .expect("cross-plane graph fixture");
        let err = validate_channels(&graph).unwrap_err();
        assert_eq!(err.kind(), "channel_invalid");
    }
}
