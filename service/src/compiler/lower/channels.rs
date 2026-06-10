//! Streaming-channel lowering (docs/25 — consumer-join model).
//!
//! An `AutomatedStep` may declare statically-typed streaming [`Channel`]s. The
//! job emits dynamic tokens into them mid-execution (`emit`/`open_output` via
//! the SDK), routed by the engine's `control_emit` effect into a synthesized
//! per-channel place. This module owns that synthesis, shared by the inline and
//! pooled AutomatedStep lowering paths so both expose identical channel
//! topology.
//!
//! The PRODUCER lowering is UNIFORM (docs/25): every control OUT channel emits
//! one bracketed episode of `open | item* | close` tokens onto ONE accumulating
//! raw place `p_{id}_{name}`. There is NO producer-side branch on a contract.
//! How that episode is FOLDED is the CONSUMER edge's decision — the
//! [`ChannelJoin`] on the edge that wires off this channel's source handle:
//!
//!   - **`Each`** (default) — a single transition `t_{id}_{name}_each`, guarded
//!     `item.kind == "item"`, consumes each `item` token and projects
//!     `item.payload` into a per-channel place `p_{id}_{name}_each`. The
//!     downstream fires once per item — this generalises the old `signal`
//!     behaviour (a "signal"/alert is just one `item`). The `open`/`close`
//!     tokens are left UNCONSUMED in the raw place (no transition matches them;
//!     harmless, no drains).
//!   - **`Gather`** — the counted barrier (the old `scatter` path). The `close`
//!     token (guarded `close.kind == "close"`) drives a coordinator; each `item`
//!     token (guarded `item.kind == "item"`) projects to a result; the shared
//!     `gather::emit_gather_barrier` collects exactly `count` items correlated on
//!     `__map_id`, sorts by `__map_idx`, and reduces to one `#{ output: [..] }`
//!     collection token on `p_{id}_{name}_gathered`. The barrier sizes itself on
//!     the episode's own `close.count` — there is no producer-side fan-out cap.
//!     `count == 0` (an empty episode: `open` then `close(0)`) fires the barrier
//!     once with `[]`.
//!
//!   - **Data** (docs/25 §2-4) — the OPEN control token (`kind: open`), carrying
//!     the out-of-band transport DESCRIPTOR, lands verbatim on `p_{id}_{name}`
//!     and flows to the edge-wired consumer EARLY (the moment `open_output` is
//!     called, mid-job — independent of producer completion). The matching CLOSE
//!     token only updates producer status; bulk bytes never enter the marking,
//!     so a data channel adds exactly one consumer-facing place and NO
//!     split/gather. Data edges never carry a `join`.
//!
//! For the OUT-direction channels we ALSO synthesize the ingestion seam:
//!   - one control inbox signal place `p_{id}_control_in` where the executor's
//!     `control_emit` event lands (the engine `ExecutorWatcher` routes it there
//!     via the job's `event_routes["control_emit"]`).
//!   - one `t_{id}_control_emit` transition draining that inbox carrying the
//!     `control_emit` engine effect, whose `effect_config.channel_routes` maps
//!     each channel name → its synthesized RAW place id. The handler reads the
//!     emit's `channel` field and deposits the token onto the resolved place.

use super::*;
use crate::models::template::{Channel, ChannelDirection, ChannelJoin, ChannelPlane, WorkflowEdge};
use std::collections::HashMap;

/// One synthesized channel place, ready to fold into the node's `NodePorts`.
pub(crate) struct ChannelPort {
    /// The declared channel name — the `sourceHandle`/`targetHandle` edges wire on.
    pub(crate) name: String,
    /// `Out` registers in `output_places`; `In` registers in `input_handles`.
    pub(crate) direction: ChannelDirection,
    /// The synthesized place edges attach to. For an `Each` control OUT channel
    /// this is the each-projected place (`item.payload` per item); for a
    /// `Gather` control OUT channel it is the GATHERED place (the counted-barrier
    /// output, one collection token); for a DATA OUT channel it is the raw deposit
    /// place the OPEN descriptor lands on; for an IN channel it is the node's
    /// main input place.
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
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Build the channel MANIFEST Rhai literal (`[#{ name, plane, element_kind }, …]`)
/// baked into the executor job spec under `spec.channels`. Matches
/// `aithericon_executor_domain::ChannelManifestEntry` — the worker validates each
/// `emit` channel name against this manifest. The producer emits one uniform
/// `open | item* | close` episode per channel, so the manifest carries NO fold
/// contract (the fold lives on the consumer edge's `join`). Empty (`[]`) when the
/// node declares no channels, so the spec stays byte-stable for channel-less
/// steps.
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
            serde_json::json!({
                "name": c.name,
                "plane": plane,
                "element_kind": element_kind,
                "transport": c.transport.wire_tag(),
            })
        })
        .collect();
    json_to_rhai_literal(&serde_json::Value::Array(entries))
}

/// Resolve each control OUT channel's CONSUMER-side [`ChannelJoin`] from the
/// graph edges. An edge consumes channel `name` of producer `node_id` when its
/// `source == node_id` and `source_handle == name`. v1 = ONE discipline per
/// channel: if a channel's consumer edges DISAGREE on `join`, that is a compile
/// error caught upstream in `validate_channels`; here we trust agreement and
/// take the first consumer edge's `join`. No consumer / unset ⇒ [`ChannelJoin`]
/// default (`Each`). Returns a map keyed by channel name (only control OUT
/// channels are folded; data + IN channels never appear).
fn resolve_channel_joins(
    node_id: &str,
    channels: &[Channel],
    edges: &[WorkflowEdge],
) -> HashMap<String, ChannelJoin> {
    let mut joins: HashMap<String, ChannelJoin> = HashMap::new();
    for ch in channels.iter().filter(|c| {
        matches!(c.direction, ChannelDirection::Out) && matches!(c.plane, ChannelPlane::Control)
    }) {
        let join = edges
            .iter()
            .find(|e| e.source == node_id && e.source_handle.as_deref() == Some(ch.name.as_str()))
            .and_then(|e| e.join)
            .unwrap_or_default();
        joins.insert(ch.name.clone(), join);
    }
    joins
}

/// Does this node declare ≥1 OUT channel (control OR data)? If so it needs the
/// `control_emit` ingestion seam (and thus the submit transition must register
/// `event_routes["control_emit"]` → the inbox). Both planes ride the same seam:
/// a control channel deposits `open`/`item`/`close` tokens, a data channel
/// deposits its `open`/`close` bracket tokens — all via `control_emit`. Mirrors
/// the `out_channels` filter in [`lower_channels`] so the two never drift.
pub(crate) fn has_out_channel(channels: &[Channel]) -> bool {
    channels
        .iter()
        .any(|c| matches!(c.direction, ChannelDirection::Out))
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
/// Builds the per-channel raw places, the `control_emit` ingestion seam (inbox +
/// effect transition with `channel_routes`), and — per CONSUMER-edge
/// [`ChannelJoin`] — either an each-projection transition (`Each`) or the counted
/// gather barrier (`Gather`). Returns the wiring ports the
/// caller folds into the node's `NodePorts`.
///
/// PRODUCER LOWERING IS UNIFORM: every control OUT channel deposits its uniform
/// `open | item* | close` episode onto one raw place `p_{id}_{name}`; the
/// split/gather sub-net is selected by the channel's consumer-edge `join`
/// (resolved from `edges` keyed by `(node_id, channel_name)`; default `Each`).
///
/// `channels` is the node's declared `Vec<Channel>` (both planes are acted on:
/// control channels carry `open`/`item`/`close` tokens, data channels carry the
/// `open`/`close` bracket — all via the one `control_emit` seam, since the
/// descriptor that opens a data channel is itself a control emission). A node
/// with no channels produces no places/transitions — AIR stays byte-stable.
///
/// `node_id` is the producer node's graph id (used with `edges` to resolve each
/// out-channel's consumer join). `edges` is the full graph edge set.
///
/// `control_in` is the pre-created inbox place from [`control_inbox`] (the
/// lifecycle's submit transition already registered its id as the
/// `control_emit` event route). It is `Some` exactly when the node has ≥1 OUT
/// channel; passing the handle in (rather than re-creating it here) keeps the
/// inbox a single place wired both as the watcher's deposit target and as this
/// fan-out transition's input.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_channels(
    ctx: &mut Context,
    id: &str,
    label: &str,
    node_id: &str,
    channels: &[Channel],
    edges: &[WorkflowEdge],
    control_in: Option<PlaceHandle<DynamicToken>>,
    input_place: &PlaceHandle<DynamicToken>,
) -> LoweredChannels {
    let mut ports: Vec<ChannelPort> = Vec::new();

    if channels.is_empty() {
        return LoweredChannels { ports };
    }

    // Per-channel CONSUMER join discipline (control OUT channels only). v1: one
    // discipline per channel; disagreement is rejected by `validate_channels`.
    let joins = resolve_channel_joins(node_id, channels, edges);

    // All OUT channels (control and data) share one ingestion seam: a control
    // inbox the executor's `control_emit` event lands on, drained by a single
    // transition that re-routes each emit by channel name. A data channel's
    // `open` descriptor token rides the SAME seam — so it joins `out_channels`
    // and gets a deposit place / output arc just like a control channel. Build
    // the seam only if any OUT channel exists.
    let out_channels: Vec<&Channel> = channels
        .iter()
        .filter(|c| matches!(c.direction, ChannelDirection::Out))
        .collect();

    // `channel_routes`: channel name → synthesized RAW DEPOSIT place id. The
    // producer always deposits its `open | item* | close` episode here; the
    // consumer-facing place is then either the each-projected place (`Each`) or
    // the gathered output (`Gather`). For a DATA channel the raw deposit place IS
    // the consumer-facing place (the OPEN descriptor lands there verbatim).
    let mut channel_routes = serde_json::Map::new();
    // Data-plane CLOSE routing (docs/25 §6): a data channel's `close` bracket
    // deposits onto a SEPARATE producer-status place, NOT the consumer-facing
    // `open` place — otherwise the consumer fires twice (once on `open`, once on
    // the subjectless `close`) and the empty close-firing races the real one and
    // can win with an empty drain. Keyed channel name → close-sink place id; only
    // data channels populate it.
    let mut channel_close_routes = serde_json::Map::new();
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
        // producer status. The `open` rides `channel_routes` to the consumer
        // place; the `close` rides `channel_close_routes` to a SEPARATE sink so
        // it never reaches the consumer (see the close-routing note above).
        if matches!(ch.plane, ChannelPlane::Data) {
            let p_chan: PlaceHandle<DynamicToken> = ctx.signal(
                format!("p_{id}_{seg}"),
                format!("{label} - Data Channel '{}'", ch.name),
            );
            channel_routes.insert(
                ch.name.clone(),
                serde_json::Value::String(p_chan.id().to_string()),
            );
            deposit_places.push(p_chan.clone());

            // CLOSE sink: a producer-status place the `close` bracket lands on,
            // NOT wired to any consumer. The consumer drains the transport to its
            // own `is_eof` (out-of-band), so it never needs the net `close`
            // token; depositing `close` here (instead of onto `p_{id}_{seg}`)
            // keeps the consumer firing EXACTLY once, on `open`.
            let p_close: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_{seg}_close"),
                format!(
                    "{label} - Data Channel '{}' Close (producer status)",
                    ch.name
                ),
            );
            channel_close_routes.insert(
                ch.name.clone(),
                serde_json::Value::String(p_close.id().to_string()),
            );
            deposit_places.push(p_close.clone());

            ports.push(ChannelPort {
                name: ch.name.clone(),
                direction: ChannelDirection::Out,
                place: p_chan,
            });
            continue;
        }

        // CONTROL OUT channel. The producer deposits a UNIFORM `open | item* |
        // close` episode onto ONE raw accumulating place `p_{id}_{name}`. The
        // CONSUMER edge's `join` decides the fold.
        let p_raw: PlaceHandle<DynamicToken> = ctx.signal(
            format!("p_{id}_{seg}"),
            format!("{label} - Channel '{}' (raw)", ch.name),
        );
        channel_routes.insert(
            ch.name.clone(),
            serde_json::Value::String(p_raw.id().to_string()),
        );
        deposit_places.push(p_raw.clone());

        let join = joins.get(&ch.name).copied().unwrap_or_default();
        match join {
            ChannelJoin::Each => {
                // ONE transition consuming each `item` token from the raw place,
                // projecting `item.payload` into a per-channel place. open/close
                // stay UNCONSUMED in the raw place — no transition matches them,
                // harmless, no drains. The downstream fires once per item (this
                // generalises the old `signal` behaviour: a signal is one item).
                let p_each: PlaceHandle<DynamicToken> = ctx.signal(
                    format!("p_{id}_{seg}_each"),
                    format!("{label} - Channel '{}' (each)", ch.name),
                );
                ctx.transition(
                    format!("t_{id}_{seg}_each"),
                    format!("{label} - Channel '{}' Each", ch.name),
                )
                .auto_input("item", &p_raw)
                .auto_output(p_each.id().to_string(), &p_each)
                .guard_rhai(r#"item.kind == "item""#)
                .logic_rhai(format!(
                    "#{{ \"{}\": item.payload }}",
                    rhai_str_escape(p_each.id())
                ))
                .done();

                // The consumer-facing place is the each-projected place.
                ports.push(ChannelPort {
                    name: ch.name.clone(),
                    direction: ChannelDirection::Out,
                    place: p_each,
                });
            }
            ChannelJoin::Gather => {
                // The counted barrier (the old scatter path). Split the raw
                // episode: `close` → coordinator, each `item` → projected result;
                // the shared barrier collects exactly `count` items correlated on
                // `__map_id`, sorts by `__map_idx`, reduces to one collection.
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

                // t_{id}_{seg}_close — consume the `close` token, emit the gather
                // coordinator `#{ count: <n>, __map_id }`. The correlate id is the
                // emit's `__map_id`. `count == 0` (empty episode) yields `count: 0`
                // and the barrier fires once with `[]` (the engine's count-gated
                // gather fires when `len >= 0`). The barrier sizes itself entirely
                // on `close.count` — there is no producer-side fan-out cap.
                ctx.transition(
                    format!("t_{id}_{seg}_close"),
                    format!("{label} - Channel '{}' Close", ch.name),
                )
                .auto_input("close", &p_raw)
                .auto_output("count", &p_count)
                .guard_rhai(r#"close.kind == "close""#)
                .logic_rhai(
                    r#"#{ count: #{ count: close.count, "__map_id": close.__map_id } }"#
                        .to_string(),
                )
                .done();

                // t_{id}_{seg}_item — consume each `item` token, project it to the
                // gather's `#{ value, __map_idx, __map_id }` shape.
                ctx.transition(
                    format!("t_{id}_{seg}_item"),
                    format!("{label} - Channel '{}' Item", ch.name),
                )
                .auto_input("item", &p_raw)
                .auto_output("result", &p_results)
                .guard_rhai(r#"item.kind == "item""#)
                .logic_rhai(
                    r#"#{ result: #{ value: item.payload, "__map_idx": item.__map_idx, "__map_id": item.__map_id } }"#
                        .to_string(),
                )
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
    for ch in channels
        .iter()
        .filter(|c| matches!(c.direction, ChannelDirection::In))
    {
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
        // Emit `channel_close_routes` only when ≥1 data channel populated it, so
        // a control-only node's AIR stays byte-stable (the handler defaults the
        // map to empty and falls back to `channel_routes`).
        let mut effect_config = serde_json::Map::new();
        effect_config.insert(
            "channel_routes".to_string(),
            serde_json::Value::Object(channel_routes),
        );
        if !channel_close_routes.is_empty() {
            effect_config.insert(
                "channel_close_routes".to_string(),
                serde_json::Value::Object(channel_close_routes),
            );
        }
        t.effect_with_config(
            effects::CONTROL_EMIT.handler_id,
            serde_json::Value::Object(effect_config),
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
    /// declaring the given `channels` array. No channel-consumer edge — control
    /// OUT channels resolve to the default `Each` join.
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

    /// `start → step → end`, where `step` declares ONE control OUT channel
    /// `chan` and a CONSUMER node `sink` (an AutomatedStep with an IN control
    /// channel of the same name) wires off `step`'s source handle carrying the
    /// given `join` (`"each"`/`"gather"`/absent). This is the path the producer
    /// lowering reads to pick the fold discipline.
    fn graph_with_consumer_join(chan: serde_json::Value, join: Option<&str>) -> WorkflowGraph {
        let name = chan["name"].as_str().unwrap().to_string();
        // Consumer IN channel mirrors the producer's plane so the only thing
        // under test is the `join` (a cross-plane edge would error first).
        let plane = chan["plane"].as_str().unwrap_or("control").to_string();
        let in_element = if plane == "data" {
            json!({ "type": "binary", "content_type": "image/jpeg" })
        } else {
            json!({ "type": "any" })
        };
        let mut consume_edge = json!({
            "id": "e_chan", "source": "step", "sourceHandle": name,
            "target": "sink", "targetHandle": name, "type": "sequence"
        });
        if let Some(j) = join {
            consume_edge["join"] = json!(j);
        }
        serde_json::from_value(json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"step","type":"automated_step","slug":"step","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Producer",
                     "executionSpec":{"backendType":"docker","config":{"image":"alpine:latest"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"executor"},
                     "channels":[chan]}},
            {"id":"sink","type":"automated_step","slug":"sink","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Consumer",
                     "executionSpec":{"backendType":"docker","config":{"image":"alpine:latest"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"executor"},
                     "channels":[{ "name": name, "direction": "in", "plane": plane,
                                   "element": in_element }]}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"step","targetHandle":"in","type":"sequence"},
            consume_edge,
            {"id":"e3","source":"sink","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }))
        .expect("consumer-join graph fixture")
    }

    fn each_channel() -> serde_json::Value {
        json!({ "name": "events", "direction": "out", "plane": "control",
                "element": { "type": "any" } })
    }

    fn gather_channel() -> serde_json::Value {
        json!({ "name": "items", "direction": "out", "plane": "control",
                "element": { "type": "any" } })
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
    fn each_channel_synthesizes_place_and_control_emit_route() {
        // No consumer edge ⇒ default Each join.
        let graph = graph_with_channels(json!([each_channel()]));
        let air =
            compile_to_air(&graph, "ch-each", "d", &std::collections::HashMap::new()).unwrap();

        let places = place_ids(&air);
        // The raw deposit place (where open|item*|close land).
        assert!(
            places.iter().any(|p| p == "p_step_events"),
            "each channel raw place missing: {places:?}"
        );
        // The each-projected consumer-facing place.
        assert!(
            places.iter().any(|p| p == "p_step_events_each"),
            "each-projected place missing: {places:?}"
        );
        assert!(
            places.iter().any(|p| p == "p_step_control_in"),
            "control inbox missing: {places:?}"
        );

        // The each transition: guard `item.kind == "item"`, projecting payload.
        let t = transition(&air, "t_step_events_each");
        let src = t["logic"]
            .get("Rhai")
            .and_then(|l| l.get("source"))
            .or_else(|| t["logic"].get("source"))
            .and_then(|s| s.as_str())
            .expect("each transition rhai source");
        assert!(
            src.contains("item.payload"),
            "each must project item.payload; got {src}"
        );
        let guard = t["guard"].as_str().or_else(|| t["guard"]["expr"].as_str());
        if let Some(g) = guard {
            assert!(
                g.contains(r#"item.kind == "item""#),
                "each guard wrong: {g}"
            );
        }

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
        // The route map points the channel at its RAW deposit place.
        assert_eq!(
            config["channel_routes"]["events"], "p_step_events",
            "channel_routes must map the channel name to its RAW place; got {config}"
        );
    }

    #[test]
    fn gather_channel_synthesizes_gather_and_split() {
        // A consumer edge carrying join: "gather" drives the gather lowering.
        let graph = graph_with_consumer_join(gather_channel(), Some("gather"));
        let air =
            compile_to_air(&graph, "ch-gather", "d", &std::collections::HashMap::new()).unwrap();

        let places = place_ids(&air);
        for expect in [
            "p_step_items",
            "p_step_items_count",
            "p_step_items_results",
            "p_step_items_gathered",
        ] {
            assert!(
                places.iter().any(|p| p == expect),
                "gather place '{expect}' missing: {places:?}"
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
        assert_eq!(config["channel_routes"]["items"], "p_step_items");
    }

    /// An `each`-join consumer edge resolves to the SAME each lowering as the
    /// default (no scatter/gather split). Proves the explicit `each` join works.
    #[test]
    fn explicit_each_join_lowers_to_each() {
        let graph = graph_with_consumer_join(each_channel(), Some("each"));
        let air = compile_to_air(
            &graph,
            "ch-each-explicit",
            "d",
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let places = place_ids(&air);
        assert!(
            places.iter().any(|p| p == "p_step_events_each"),
            "each place missing: {places:?}"
        );
        for absent in ["p_step_events_count", "p_step_events_gathered"] {
            assert!(
                !places.iter().any(|p| p == absent),
                "each join must not synthesize gather place '{absent}': {places:?}"
            );
        }
        transition(&air, "t_step_events_each");
    }

    /// The gather close transition guards on the COLLAPSED `close` kind (not the
    /// retired `scatter_close`) and the item transition on `item`.
    #[test]
    fn gather_split_uses_collapsed_kind_strings() {
        let graph = graph_with_consumer_join(gather_channel(), Some("gather"));
        let air = compile_to_air(
            &graph,
            "ch-gather-kinds",
            "d",
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let close = transition(&air, "t_step_items_close");
        let guard = close["guard"]
            .as_str()
            .or_else(|| close["guard"]["expr"].as_str());
        if let Some(g) = guard {
            assert!(
                g.contains(r#"close.kind == "close""#),
                "close guard wrong: {g}"
            );
            assert!(!g.contains("scatter_close"), "retired kind leaked: {g}");
        }
        let item = transition(&air, "t_step_items_item");
        let iguard = item["guard"]
            .as_str()
            .or_else(|| item["guard"]["expr"].as_str());
        if let Some(g) = iguard {
            assert!(
                g.contains(r#"item.kind == "item""#),
                "item guard wrong: {g}"
            );
            assert!(!g.contains("scatter_item"), "retired kind leaked: {g}");
        }
    }

    /// `t_{id}_control_emit` MUST declare each channel's deposit place as an
    /// OUTPUT ARC (port name == place id). The engine validates the handler's
    /// returned token keys (place ids) against the transition's output ports
    /// (`firing.rs` → `UnknownOutputPort`); without the arc the live emit path
    /// NetFails with "Unknown output port 'p_..._raw' returned by script".
    /// Regression guard for the demo-17 live-checkpoint failure.
    #[test]
    fn control_emit_declares_deposit_place_as_output_arc() {
        // The RAW deposit place (where the uniform open|item*|close lands) must
        // be a control_emit output arc, for BOTH join disciplines. Each: default
        // join (no consumer edge); Gather: a gather consumer edge.
        for (label, graph, deposit) in [
            (
                "each",
                graph_with_channels(json!([each_channel()])),
                "p_step_events",
            ),
            (
                "gather",
                graph_with_consumer_join(gather_channel(), Some("gather")),
                "p_step_items",
            ),
        ] {
            let air = compile_to_air(&graph, "ch-arc", "d", &std::collections::HashMap::new())
                .unwrap_or_else(|e| panic!("{label} compile failed: {e:?}"));
            let t = transition(&air, "t_step_control_emit");
            let outs = t["outputs"]
                .as_array()
                .unwrap_or_else(|| panic!("{label}: t_step_control_emit has no output arcs: {t}"));
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
        let graph = graph_with_channels(json!([each_channel()]));
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

    /// The gather ITEM transition projects each item to the gather's
    /// `#{ value, __map_idx, __map_id }` shape with NO producer-side cap — the
    /// barrier sizes itself on `close.count`, so there is no over-fanout throw.
    #[test]
    fn gather_item_projects_without_fanout_cap() {
        let graph = graph_with_consumer_join(gather_channel(), Some("gather"));
        let air =
            compile_to_air(&graph, "ch-guard", "d", &std::collections::HashMap::new()).unwrap();

        let item = transition(&air, "t_step_items_item");
        let src = item["logic"]
            .get("Rhai")
            .and_then(|l| l.get("source"))
            .or_else(|| item["logic"].get("source"))
            .and_then(|s| s.as_str())
            .expect("item transition rhai source");
        assert!(
            src.contains("__map_idx") && src.contains("__map_id"),
            "item transition must project the gather shape; got {src}"
        );
        assert!(
            !src.contains("throw"),
            "item transition must NOT carry a fan-out cap (no throw); got {src}"
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
        let air = compile_to_air(
            &graph,
            "ch-data-arc",
            "d",
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let t = transition(&air, "t_step_control_emit");
        let outs = t["outputs"]
            .as_array()
            .unwrap_or_else(|| panic!("t_step_control_emit has no output arcs: {t}"));
        assert!(
            outs.iter().any(|a| a["place"] == "p_step_frames"),
            "control_emit must declare an output arc to the data deposit place 'p_step_frames'; got {outs:?}"
        );
    }

    /// A data channel's `close` bracket MUST route to a SEPARATE producer-status
    /// place (`p_{id}_{name}_close`), NOT the consumer-facing `open` place — else
    /// the consumer fires twice (on `open` and on the subjectless `close`) and the
    /// empty close-firing races + can win with an empty drain (the bug the
    /// audio-transcribe demo exposed live). Regression guard: assert the separate
    /// place exists, is a control_emit output arc, and that `channel_close_routes`
    /// points the channel at it while `channel_routes` keeps the consumer place.
    #[test]
    fn data_channel_close_routes_to_separate_sink() {
        let graph = graph_with_channels(json!([data_channel()]));
        let air = compile_to_air(
            &graph,
            "ch-data-close",
            "d",
            &std::collections::HashMap::new(),
        )
        .unwrap();

        let places = place_ids(&air);
        assert!(
            places.iter().any(|p| p == "p_step_frames_close"),
            "data channel must synthesize a separate close sink place: {places:?}"
        );

        let t = transition(&air, "t_step_control_emit");
        let outs = t["outputs"].as_array().expect("control_emit output arcs");
        assert!(
            outs.iter().any(|a| a["place"] == "p_step_frames_close"),
            "control_emit must declare an output arc to the close sink 'p_step_frames_close'; got {outs:?}"
        );

        let config = t["logic"]
            .get("Effect")
            .and_then(|e| e.get("config"))
            .or_else(|| t["logic"].get("config"))
            .expect("config present");
        assert_eq!(
            config["channel_routes"]["frames"], "p_step_frames",
            "open routes to the consumer-facing place"
        );
        assert_eq!(
            config["channel_close_routes"]["frames"], "p_step_frames_close",
            "close routes to the separate sink, NOT the consumer place"
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
        let air = compile_to_air(
            &graph,
            "ch-data-wire",
            "d",
            &std::collections::HashMap::new(),
        )
        .expect("data channel must be edge-wireable off its source handle");
        let places = place_ids(&air);
        assert!(
            places.iter().any(|p| p == "p_step_frames"),
            "producer data place (the merge survivor the consumer reads) missing: {places:?}"
        );
    }

    #[test]
    fn manifest_is_baked_into_job_spec() {
        let graph = graph_with_channels(json!([each_channel(), gather_channel()]));
        let air = compile_to_air(
            &graph,
            "ch-manifest",
            "d",
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let prepare = transition(&air, "step/prepare");
        let src = prepare["logic"]
            .get("Rhai")
            .and_then(|l| l.get("source"))
            .or_else(|| prepare["logic"].get("source"))
            .and_then(|s| s.as_str())
            .expect("prepare rhai source");
        assert!(
            src.contains(r#""channels":"#),
            "manifest key missing: {src}"
        );
        assert!(src.contains(r#""name": "events""#), "events entry missing");
        assert!(src.contains(r#""name": "items""#), "items entry missing");
        // The manifest carries NO fold contract — the fold lives on the consumer
        // edge's join, not the producer's channel.
        assert!(
            !src.contains(r#""contract":"#),
            "manifest must not carry a contract entry; got {src}"
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
        let graph = graph_with_channels(json!([each_channel(), each_channel()]));
        let err = validate_channels(&graph).unwrap_err();
        assert_eq!(err.kind(), "channel_invalid");
    }

    /// A `gather` consumer edge is VALID with no producer-side cap — the counted
    /// barrier sizes itself on the episode's own `close.count`.
    #[test]
    fn validate_accepts_gather_without_producer_cap() {
        let graph = graph_with_consumer_join(
            json!({ "name": "items", "direction": "out", "plane": "control",
                    "element": { "type": "any" } }),
            Some("gather"),
        );
        assert!(validate_channels(&graph).is_ok());
    }

    /// A bare control channel with no consumer is VALID — it defaults to the
    /// `Each` join (the producer is uniform; there are no producer-side knobs).
    #[test]
    fn validate_accepts_bare_control_channel() {
        let graph = graph_with_channels(json!([
            { "name": "events", "direction": "out", "plane": "control",
              "element": { "type": "any" } }
        ]));
        assert!(validate_channels(&graph).is_ok());
    }

    /// A `join` set on a non-channel (or data) edge is rejected.
    #[test]
    fn validate_rejects_join_on_data_edge() {
        let graph = graph_with_consumer_join(data_channel(), Some("each"));
        let err = validate_channels(&graph).unwrap_err();
        assert_eq!(err.kind(), "channel_invalid");
    }

    /// A `join` on an edge whose source handle names a channel that is NOT a
    /// control OUT channel (here: an IN-direction control channel) is
    /// rejected — the fold discipline only exists for control OUT episodes,
    /// so the join would silently never apply.
    #[test]
    fn validate_rejects_join_when_source_handle_is_not_control_out() {
        let graph: WorkflowGraph = serde_json::from_value(json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"step","type":"automated_step","slug":"step","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Producer",
                     "executionSpec":{"backendType":"docker","config":{"image":"alpine:latest"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"executor"},
                     "channels":[{ "name": "events", "direction": "in", "plane": "control",
                                   "element": { "type": "any" } }]}},
            {"id":"sink","type":"automated_step","slug":"sink","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Consumer",
                     "executionSpec":{"backendType":"docker","config":{"image":"alpine:latest"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"executor"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"step","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"step","sourceHandle":"events","target":"sink",
             "targetHandle":"in","type":"sequence","join":"each"},
            {"id":"e3","source":"sink","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }))
        .expect("in-channel join graph fixture");
        let err = validate_channels(&graph).unwrap_err();
        assert_eq!(err.kind(), "channel_invalid");
    }

    #[test]
    fn validate_rejects_unresolved_json_element_schema() {
        let graph = graph_with_channels(json!([
            { "name": "events", "direction": "out", "plane": "control",
              "element": { "type": "json", "schema": { "$ref": "#/definitions/Missing" } } }
        ]));
        let err = validate_channels(&graph).unwrap_err();
        assert_eq!(err.kind(), "channel_invalid");
    }

    #[test]
    fn validate_accepts_well_formed_channels() {
        // each (no consumer ⇒ default Each) + gather (gather consumer edge).
        let graph = graph_with_consumer_join(gather_channel(), Some("gather"));
        assert!(validate_channels(&graph).is_ok());
        let graph = graph_with_channels(json!([each_channel()]));
        assert!(validate_channels(&graph).is_ok());
    }

    #[test]
    fn validate_accepts_well_formed_data_channel() {
        let graph = graph_with_channels(json!([data_channel()]));
        assert!(validate_channels(&graph).is_ok());
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
                                   "element": { "type": "any" } }]}},
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
