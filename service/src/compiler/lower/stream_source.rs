//! `WorkflowNodeData::StreamSource` lowering — workflow-as-streaming-endpoint
//! INGRESS (docs/25 §9 Phase 3).
//!
//! A StreamSource is an AutomatedStep's channel surface WITHOUT the executor:
//! there is no job, no lifecycle, no submit transition. The external producer
//! publishes `ControlEmitEvent`s through a mekhan ingress endpoint; the engine
//! `ExecutorWatcher` (`handle_control_emit`) resolves the deposit target from
//! the event's METADATA routing (`event_routes["control_emit"]`) — for an
//! AutomatedStep those tags are registered by the submit transition, for a
//! StreamSource the INGRESS stamps them onto every published emit. The place
//! it must target is the node's control inbox:
//!
//! ```text
//!     p_{id}_control_in            ← ingress-published ControlEmitEvents land here
//!         │ t_{id}_control_emit    ← fan-out by channel name (channel_routes)
//!         ▼
//!     p_{id}_{name}                ← raw episode (open | item* | close) per channel
//!         │ each/gather fold       ← consumer-edge join discipline, unchanged
//!         ▼
//!     p_{id}_{name}_each / _gathered / (data: p_{id}_{name} verbatim)
//! ```
//!
//! Everything below the inbox reuses [`super::channels`] VERBATIM — the same
//! synthesis the AutomatedStep paths share — so a StreamSource's channel
//! topology can never drift from a step's.
//!
//! The place-id convention `p_{id}_control_in` is therefore a RUNTIME contract
//! with the ingress (it has no submit transition to read routes from); the
//! conformance test below pins it.

use super::*;

pub(crate) fn lower_stream_source(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::StreamSource {
        label, channels, ..
    } = &cx.node.data
    else {
        unreachable!("lower_stream_source on non-StreamSource node")
    };
    // Cloned before the `&mut *cx.ctx` reborrow, mirroring the AutomatedStep
    // lowering's borrow discipline.
    let channels = channels.clone();
    let label = label.clone();

    let ctx = &mut *cx.ctx;

    // The control inbox IS the node's entry: the only way tokens enter this
    // sub-graph is the ingress-published `control_emit` deposit. Validation
    // (`validate_stream_source`) guarantees ≥1 Out channel, so `control_inbox`
    // always mints the place; a `None` here is an internal invariant break.
    let p_control_in =
        super::channels::control_inbox(ctx, id, &label, &channels).ok_or_else(|| {
            CompileError::Compilation(format!(
                "internal: stream_source '{id}' has no Out channel — \
                 validate_stream_source must reject this before lowering"
            ))
        })?;

    // Reuse the shared channel synthesis verbatim: raw deposit place per OUT
    // channel, each/gather consumer folds, data-plane close sink, and the
    // `t_{id}_control_emit` fan-out carrying `channel_routes`. A StreamSource
    // declares no In channel (validated), so the IN-alias branch never runs —
    // passing the inbox as the "main input place" is inert.
    let lowered = super::channels::lower_channels(
        ctx,
        id,
        &label,
        &cx.node.id,
        &channels,
        &cx.graph.edges,
        Some(p_control_in.clone()),
        &p_control_in,
    );

    // Every channel port is a source-handle output (edges wire off
    // `sourceHandle == name`). No default output, no terminals, no data port —
    // a StreamSource produces only its channel streams.
    let mut output_places: Vec<(Option<String>, PlaceHandle<DynamicToken>)> = Vec::new();
    for port in lowered.ports {
        match port.direction {
            crate::models::template::ChannelDirection::Out => {
                output_places.push((Some(port.name), port.place));
            }
            crate::models::template::ChannelDirection::In => {
                // Unreachable post-validation; ignore defensively rather than
                // alias a place validation already rejected.
            }
        }
    }

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_control_in,
            output_places,
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface();
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::compiler::compile::{compile_to_air_with_options, CompileOptions};
    use crate::compiler::compile_to_air;
    use crate::compiler::error::CompileError;
    use crate::models::template::WorkflowGraph;
    use serde_json::json;

    /// A graph where `src` is a StreamSource declaring `channels`, wired to a
    /// consumer AutomatedStep `step` declaring the matching In channel. The
    /// `start → end` control path satisfies the one-Start/one-End invariant;
    /// the streaming sub-graph hangs off the StreamSource root (reachability
    /// roots at StreamSource nodes too — they're external entry points).
    fn graph_with_source(
        channels: serde_json::Value,
        consumer_in: serde_json::Value,
        channel_edge: serde_json::Value,
    ) -> WorkflowGraph {
        serde_json::from_value(json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"src","type":"stream_source","slug":"src","position":{"x":0,"y":0},
             "data":{"type":"stream_source","label":"Ingress","channels": channels}},
            {"id":"step","type":"automated_step","slug":"step","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Consumer",
                     "executionSpec":{"backendType":"docker","config":{"image":"alpine:latest"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"executor"},
                     "channels":[consumer_in]}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"end","targetHandle":"in","type":"sequence"},
            channel_edge
          ]
        }))
        .expect("stream_source graph fixture")
    }

    fn control_fixture() -> WorkflowGraph {
        graph_with_source(
            json!([{ "name": "events", "direction": "out", "plane": "control",
                     "element": { "type": "any" } }]),
            json!({ "name": "events", "direction": "in", "plane": "control",
                    "element": { "type": "any" } }),
            json!({ "id": "e2", "source": "src", "sourceHandle": "events",
                    "target": "step", "targetHandle": "events", "type": "sequence" }),
        )
    }

    fn data_fixture() -> WorkflowGraph {
        graph_with_source(
            json!([{ "name": "frames", "direction": "out", "plane": "data",
                     "element": { "type": "binary", "content_type": "image/jpeg" } }]),
            json!({ "name": "frames", "direction": "in", "plane": "data",
                    "element": { "type": "binary", "content_type": "image/jpeg" } }),
            json!({ "id": "e2", "source": "src", "sourceHandle": "frames",
                    "target": "step", "targetHandle": "frames", "type": "sequence" }),
        )
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

    /// CONFORMANCE — the control-inbox place id is the LITERAL
    /// `p_{node_id}_control_in`. The mekhan ingress runtime has no submit
    /// transition to learn routes from: it stamps
    /// `event_routes["control_emit"] → p_{node_id}_control_in` into every
    /// published ControlEmitEvent's metadata, and the engine ExecutorWatcher
    /// (`handle_control_emit`) deposits onto exactly that place. Changing this
    /// format string silently severs every deployed streaming endpoint —
    /// update the ingress + this test TOGETHER or not at all.
    #[test]
    fn control_inbox_place_id_is_pinned_for_the_ingress() {
        let air = compile_to_air(
            &control_fixture(),
            "ss-pin",
            "d",
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let node_id = "src";
        let expected = format!("p_{node_id}_control_in");
        assert!(
            place_ids(&air).contains(&expected),
            "ingress contract place '{expected}' missing: {:?}",
            place_ids(&air)
        );
    }

    /// A control Out channel synthesizes the same topology as an
    /// AutomatedStep's: raw deposit place, each-fold place + transition, and
    /// the `t_{id}_control_emit` fan-out whose `channel_routes` targets the
    /// raw place. No executor lifecycle places/transitions exist.
    #[test]
    fn control_channel_synthesizes_shared_topology_without_lifecycle() {
        let air = compile_to_air(
            &control_fixture(),
            "ss-ctl",
            "d",
            &std::collections::HashMap::new(),
        )
        .unwrap();

        let places = place_ids(&air);
        for expect in ["p_src_control_in", "p_src_events"] {
            assert!(
                places.iter().any(|p| p == expect),
                "place '{expect}' missing: {places:?}"
            );
        }
        // The each-fold transition exists (the each-projected place itself is
        // the consumer-facing output; with a sole-consumer edge wire.rs may
        // merge the consumer's input INTO it, so assert the transition's
        // declared output rather than the place list).
        let each = transition(&air, "t_src_events_each");
        assert!(
            each["outputs"].as_array().is_some_and(|o| !o.is_empty()),
            "each transition must declare its projected output: {each}"
        );

        let t = transition(&air, "t_src_control_emit");
        let config = t["logic"]
            .get("Effect")
            .and_then(|e| e.get("config"))
            .or_else(|| t["logic"].get("config"))
            .expect("control_emit effect config");
        assert_eq!(
            config["channel_routes"]["events"], "p_src_events",
            "channel_routes must map the channel at its RAW deposit place; got {config}"
        );

        // No executor lifecycle: a StreamSource has no job. The submit
        // transition is the lifecycle's entry — its absence proves the whole
        // chain is absent.
        assert!(
            !air["transitions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == "src/submit"),
            "stream_source must not lower an executor lifecycle"
        );
    }

    /// A data Out channel gets the consumer-facing deposit place plus the
    /// separate close sink, exactly like an AutomatedStep data channel.
    #[test]
    fn data_channel_synthesizes_deposit_and_close_sink() {
        let air = compile_to_air(
            &data_fixture(),
            "ss-data",
            "d",
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let places = place_ids(&air);
        for expect in ["p_src_control_in", "p_src_frames", "p_src_frames_close"] {
            assert!(
                places.iter().any(|p| p == expect),
                "place '{expect}' missing: {places:?}"
            );
        }
        let t = transition(&air, "t_src_control_emit");
        let config = t["logic"]
            .get("Effect")
            .and_then(|e| e.get("config"))
            .or_else(|| t["logic"].get("config"))
            .expect("config present");
        assert_eq!(config["channel_routes"]["frames"], "p_src_frames");
        assert_eq!(
            config["channel_close_routes"]["frames"], "p_src_frames_close",
            "data close must route to the separate producer-status sink"
        );
    }

    /// The published interface's `entry` is the control inbox (the ingress
    /// deposit target) and the channel surfaces as a named output.
    #[test]
    fn interface_entry_is_the_control_inbox() {
        let artifacts = compile_to_air_with_options(
            &control_fixture(),
            "ss-iface",
            "d",
            &std::collections::HashMap::new(),
            CompileOptions::default(),
        )
        .unwrap();
        let iface = &artifacts.interfaces["src"];
        assert_eq!(
            iface["entry"], "p_src_control_in",
            "interface entry must be the ingress deposit place; got {iface}"
        );
        assert!(
            iface["outputs"].get("edge:events").is_some(),
            "channel must surface as a named output; got {}",
            iface["outputs"]
        );
        assert!(
            iface["data_port"].is_null(),
            "stream_source parks nothing; got {}",
            iface["data_port"]
        );
    }

    // ── Validation rejections (validate_stream_source) ──────────────────────

    fn compile_err(graph: &WorkflowGraph) -> CompileError {
        compile_to_air(graph, "ss-err", "d", &std::collections::HashMap::new())
            .expect_err("graph must be rejected")
    }

    #[test]
    fn validate_rejects_source_without_channels() {
        let graph = graph_with_source(
            json!([]),
            json!({ "name": "events", "direction": "in", "plane": "control",
                    "element": { "type": "any" } }),
            // No channel edge — there's no channel to wire.
            json!({ "id": "e2", "source": "start", "target": "step",
                    "targetHandle": "in", "type": "sequence" }),
        );
        let err = compile_err(&graph);
        assert_eq!(err.kind(), "validation", "got {err:?}");
        assert!(err.to_string().contains("at least one"), "got {err}");
    }

    #[test]
    fn validate_rejects_in_direction_channel() {
        let graph = graph_with_source(
            json!([{ "name": "events", "direction": "in", "plane": "control",
                     "element": { "type": "any" } }]),
            json!({ "name": "events", "direction": "in", "plane": "control",
                    "element": { "type": "any" } }),
            json!({ "id": "e2", "source": "start", "target": "step",
                    "targetHandle": "in", "type": "sequence" }),
        );
        let err = compile_err(&graph);
        assert_eq!(err.kind(), "channel_invalid", "got {err:?}");
    }

    /// v1 ingress transports are `jetstream` | `nats-latest` only — `s3` and
    /// `livekit` are rejected with a transport-naming message.
    #[test]
    fn validate_rejects_s3_and_livekit_transports() {
        for transport in ["s3", "livekit"] {
            let graph = graph_with_source(
                json!([{ "name": "frames", "direction": "out", "plane": "data",
                         "element": { "type": "binary", "content_type": "image/jpeg" },
                         "transport": transport }]),
                json!({ "name": "frames", "direction": "in", "plane": "data",
                        "element": { "type": "binary", "content_type": "image/jpeg" } }),
                json!({ "id": "e2", "source": "src", "sourceHandle": "frames",
                        "target": "step", "targetHandle": "frames", "type": "sequence" }),
            );
            let err = compile_err(&graph);
            assert_eq!(err.kind(), "channel_invalid", "{transport}: got {err:?}");
            assert!(
                err.to_string().contains(transport),
                "{transport}: message must name the rejected transport; got {err}"
            );
        }
    }

    #[test]
    fn validate_rejects_inbound_edge() {
        let mut graph = control_fixture();
        graph.edges.push(
            serde_json::from_value(json!({
                "id": "e_bad", "source": "start", "target": "src",
                "targetHandle": "in", "type": "sequence"
            }))
            .unwrap(),
        );
        let err = compile_err(&graph);
        assert_eq!(err.kind(), "validation", "got {err:?}");
        assert!(err.to_string().contains("inbound"), "got {err}");
    }

    /// An outgoing edge that does NOT wire off a declared channel handle (a
    /// plain control-flow edge) is rejected — a StreamSource has no
    /// control-flow output in v1.
    #[test]
    fn validate_rejects_non_channel_outbound_edge() {
        let mut graph = control_fixture();
        graph.edges.push(
            serde_json::from_value(json!({
                "id": "e_bad", "source": "src", "target": "step",
                "targetHandle": "in", "type": "sequence"
            }))
            .unwrap(),
        );
        let err = compile_err(&graph);
        assert_eq!(err.kind(), "validation", "got {err:?}");
        assert!(err.to_string().contains("channel handle"), "got {err}");
    }

    /// A source with no consumer edge still compiles (default Each fold) —
    /// the streaming sub-graph is reachable by virtue of the StreamSource
    /// being an external entry point.
    #[test]
    fn source_without_consumer_edge_compiles() {
        let graph: WorkflowGraph = serde_json::from_value(json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"src","type":"stream_source","slug":"src","position":{"x":0,"y":0},
             "data":{"type":"stream_source","label":"Ingress",
                     "channels":[{ "name": "events", "direction": "out",
                                   "plane": "control", "element": { "type": "any" } }]}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }))
        .expect("bare source fixture");
        let air =
            compile_to_air(&graph, "ss-bare", "d", &std::collections::HashMap::new()).unwrap();
        assert!(
            place_ids(&air).iter().any(|p| p == "p_src_control_in"),
            "bare stream_source must still mint its inbox"
        );
    }
}
