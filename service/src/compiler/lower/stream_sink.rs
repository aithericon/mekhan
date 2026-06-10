//! `WorkflowNodeData::StreamSink` lowering — workflow-as-streaming-endpoint
//! EGRESS (docs/25 §9 Phase 3).
//!
//! A StreamSink terminates exactly ONE In channel at a mekhan egress endpoint
//! (cardinality enforced by `validate_stream_sink`). The upstream producer
//! edge wires `targetHandle == <channel name>` into the node's single input
//! place `p_{id}_in` (the channel handle ALIASES it, mirroring the IN-channel
//! alias branch in [`super::channels::lower_channels`] — a separate inbound
//! place would receive tokens nothing consumes).
//!
//! What arrives there depends on the channel's PLANE, so the lowering is
//! plane-conditional:
//!
//! - **Data** — the upstream's OPEN descriptor token (the out-of-band
//!   transport pointer; the close bracket rides the producer's separate close
//!   sink and never reaches a consumer). `t_{id}_capture` consumes it and
//!   parks it WRITE-ONCE in `p_{id}_data`, published as the interface's
//!   `data_port` like every other parked producer — that is the egress
//!   resolution path: the step-executions projector captures the parked
//!   descriptor, and the mekhan egress endpoint resolves the live transport
//!   subject from it.
//! - **Control** — a stream of fold-projected tokens (one per `item` under
//!   the default `each` join). `t_{id}_drain` consumes each into the
//!   accumulating `p_{id}_seen` place so tokens never strand in the input
//!   place (a token left unconsumed would hold the net non-quiescent).
//!
//! No outputs, no terminals, no executor lifecycle: the sink is a pure
//! net-edge of the streaming endpoint surface.

use super::*;
use crate::models::template::{ChannelDirection, ChannelPlane};

pub(crate) fn lower_stream_sink(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::StreamSink {
        label, channels, ..
    } = &cx.node.data
    else {
        unreachable!("lower_stream_sink on non-StreamSink node")
    };
    // Validation (`validate_stream_sink`) enforces exactly one In channel;
    // re-check here so an internal caller bypassing validation fails loudly
    // instead of lowering a half-wired sink.
    let ch = match channels.as_slice() {
        [ch] if matches!(ch.direction, ChannelDirection::In) => ch.clone(),
        _ => {
            return Err(CompileError::Compilation(format!(
                "internal: stream_sink '{id}' must declare exactly one In channel — \
                 validate_stream_sink must reject this before lowering"
            )))
        }
    };
    let label = label.clone();

    let ctx = &mut *cx.ctx;

    // The single input place. The upstream channel edge usually has this as
    // its sole inbound edge, in which case wire.rs MERGES it into the
    // producer's consumer-facing place (the survivor) — the transitions below
    // then consume directly off the producer's place, which is exactly right.
    let p_in: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_in"), format!("{label} - Input"));

    let data_place_id = match ch.plane {
        ChannelPlane::Data => {
            // Park the OPEN descriptor write-once. `p_{id}_data` + the
            // interface `data_port` mirror `split_outputs`' parked-producer
            // contract (place id format included) so the step-executions
            // projector and any `<slug>.<field>` borrow treat the sink like
            // every other parked producer.
            let p_data: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_data"),
                format!("{label} - Parked Descriptor (write-once)"),
            );
            ctx.transition(
                format!("t_{id}_capture"),
                format!("{label} - Capture Descriptor (park write-once)"),
            )
            .auto_input("tok", &p_in)
            .auto_output("data", &p_data)
            .logic_rhai("#{ data: tok }".to_string())
            .done();
            Some(format!("p_{id}_data"))
        }
        ChannelPlane::Control => {
            // Drain every arriving fold-projected token into the accumulating
            // seen place. No business consumer reads it in v1 — the point is
            // that nothing strands in the input place.
            let p_seen: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_seen"),
                format!("{label} - Seen (accumulating)"),
            );
            ctx.transition(format!("t_{id}_drain"), format!("{label} - Drain"))
                .auto_input("tok", &p_in)
                .auto_output("seen", &p_seen)
                .logic_rhai("#{ seen: tok }".to_string())
                .done();
            None
        }
    };

    // The channel handle aliases the input place (the IN-channel alias
    // convention from `channels::lower_channels`): the upstream edge's
    // `targetHandle == <name>` resolves through `input_handles` to `p_in`.
    let mut input_handles = HashMap::new();
    input_handles.insert(ch.name.clone(), p_in.clone());

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_in,
            output_places: Vec::new(),
            input_places: HashMap::new(),
            input_handles,
        },
    );
    let iface = cx.publish_interface();
    iface.data_port = data_place_id;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::compiler::compile::{compile_to_air_with_options, CompileOptions};
    use crate::compiler::compile_to_air;
    use crate::compiler::error::CompileError;
    use crate::models::template::WorkflowGraph;
    use serde_json::json;

    /// `start → producer → end` where `producer` (an AutomatedStep) declares
    /// one Out channel and wires it into `egress`, a StreamSink declaring the
    /// matching In `sink_channels` entry/entries.
    fn graph_with_sink(
        producer_channel: serde_json::Value,
        sink_channels: serde_json::Value,
        channel_edge: serde_json::Value,
    ) -> WorkflowGraph {
        serde_json::from_value(json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"producer","type":"automated_step","slug":"producer","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Producer",
                     "executionSpec":{"backendType":"docker","config":{"image":"alpine:latest"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"executor"},
                     "channels":[producer_channel]}},
            {"id":"egress","type":"stream_sink","slug":"egress","position":{"x":0,"y":0},
             "data":{"type":"stream_sink","label":"Egress","channels": sink_channels}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"producer","targetHandle":"in","type":"sequence"},
            channel_edge,
            {"id":"e3","source":"producer","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }))
        .expect("stream_sink graph fixture")
    }

    fn control_fixture() -> WorkflowGraph {
        graph_with_sink(
            json!({ "name": "events", "direction": "out", "plane": "control",
                    "element": { "type": "any" } }),
            json!([{ "name": "events", "direction": "in", "plane": "control",
                     "element": { "type": "any" } }]),
            json!({ "id": "e2", "source": "producer", "sourceHandle": "events",
                    "target": "egress", "targetHandle": "events", "type": "sequence" }),
        )
    }

    fn data_fixture() -> WorkflowGraph {
        graph_with_sink(
            json!({ "name": "frames", "direction": "out", "plane": "data",
                    "element": { "type": "binary", "content_type": "image/jpeg" } }),
            json!([{ "name": "frames", "direction": "in", "plane": "data",
                     "element": { "type": "binary", "content_type": "image/jpeg" } }]),
            json!({ "id": "e2", "source": "producer", "sourceHandle": "frames",
                    "target": "egress", "targetHandle": "frames", "type": "sequence" }),
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

    /// Data-plane sink: `t_{id}_capture` consumes the descriptor off the
    /// (merge-surviving) producer place and parks it in `p_{id}_data`. The
    /// sink's `p_{id}_in` is the dead half of a pass-through merge — the
    /// capture's input arc must point at the producer's consumer-facing place.
    #[test]
    fn data_sink_captures_descriptor_into_parked_place() {
        let air = compile_to_air(
            &data_fixture(),
            "sk-data",
            "d",
            &std::collections::HashMap::new(),
        )
        .unwrap();

        let places = place_ids(&air);
        assert!(
            places.iter().any(|p| p == "p_egress_data"),
            "parked descriptor place missing: {places:?}"
        );

        let t = transition(&air, "t_egress_capture");
        let inputs = t["inputs"].as_array().expect("capture input arcs");
        assert_eq!(inputs.len(), 1, "capture consumes exactly one place: {t}");
        // Sole-inbound-edge pass-through merge: p_egress_in dies, the
        // producer's data deposit place survives.
        assert_eq!(
            inputs[0]["place"], "p_producer_frames",
            "capture must consume the merge-surviving producer place; got {inputs:?}"
        );
        let outputs = t["outputs"].as_array().expect("capture output arcs");
        assert!(
            outputs.iter().any(|a| a["place"] == "p_egress_data"),
            "capture must park into p_egress_data; got {outputs:?}"
        );
    }

    /// Data-plane sink publishes `data_port = p_{id}_data` — the egress
    /// resolution path: the step-executions projector captures the parked
    /// descriptor off this port, and the mekhan egress endpoint resolves the
    /// live transport subject from it.
    #[test]
    fn data_sink_publishes_data_port() {
        let artifacts = compile_to_air_with_options(
            &data_fixture(),
            "sk-iface",
            "d",
            &std::collections::HashMap::new(),
            CompileOptions::default(),
        )
        .unwrap();
        let iface = &artifacts.interfaces["egress"];
        assert_eq!(
            iface["data_port"], "p_egress_data",
            "sink must park the descriptor like other parked producers; got {iface}"
        );
        // The channel handle is a named input; there are no outputs.
        assert!(
            iface["named_inputs"].get("frames").is_some(),
            "channel handle must be a named input; got {}",
            iface["named_inputs"]
        );
        assert!(
            iface["outputs"].as_object().is_some_and(|o| o.is_empty()),
            "stream_sink has no outputs; got {}",
            iface["outputs"]
        );
    }

    /// Control-plane sink: `t_{id}_drain` consumes each fold-projected token
    /// into the accumulating `p_{id}_seen` place, and NO data port is
    /// published (nothing is parked write-once).
    #[test]
    fn control_sink_drains_into_seen_place_without_data_port() {
        let artifacts = compile_to_air_with_options(
            &control_fixture(),
            "sk-ctl",
            "d",
            &std::collections::HashMap::new(),
            CompileOptions::default(),
        )
        .unwrap();

        let places = place_ids(&artifacts.air);
        assert!(
            places.iter().any(|p| p == "p_egress_seen"),
            "accumulating seen place missing: {places:?}"
        );
        assert!(
            !places.iter().any(|p| p == "p_egress_data"),
            "control sink must not mint a parked-data place: {places:?}"
        );

        let t = transition(&artifacts.air, "t_egress_drain");
        let inputs = t["inputs"].as_array().expect("drain input arcs");
        // The sole-inbound-edge merge resolves the sink's input to the
        // producer's each-projected place.
        assert_eq!(
            inputs[0]["place"], "p_producer_events_each",
            "drain must consume the merge-surviving each place; got {inputs:?}"
        );

        assert!(
            artifacts.interfaces["egress"]["data_port"].is_null(),
            "control sink publishes no data_port; got {}",
            artifacts.interfaces["egress"]["data_port"]
        );
    }

    // ── Validation rejections (validate_stream_sink) ─────────────────────────

    fn compile_err(graph: &WorkflowGraph) -> CompileError {
        compile_to_air(graph, "sk-err", "d", &std::collections::HashMap::new())
            .expect_err("graph must be rejected")
    }

    #[test]
    fn validate_rejects_zero_and_two_channels() {
        for channels in [
            json!([]),
            json!([
                { "name": "a", "direction": "in", "plane": "control",
                  "element": { "type": "any" } },
                { "name": "b", "direction": "in", "plane": "control",
                  "element": { "type": "any" } }
            ]),
        ] {
            let graph = graph_with_sink(
                json!({ "name": "events", "direction": "out", "plane": "control",
                        "element": { "type": "any" } }),
                channels.clone(),
                // Keep the edge off the channel handles so cardinality (not
                // wiring) is the first failure.
                json!({ "id": "e2", "source": "producer", "sourceHandle": "events",
                        "target": "egress", "targetHandle": "a", "type": "sequence" }),
            );
            let err = compile_err(&graph);
            assert_eq!(err.kind(), "validation", "channels={channels}: got {err:?}");
            assert!(
                err.to_string().contains("exactly one"),
                "channels={channels}: got {err}"
            );
        }
    }

    #[test]
    fn validate_rejects_out_direction_channel() {
        let graph = graph_with_sink(
            json!({ "name": "events", "direction": "out", "plane": "control",
                    "element": { "type": "any" } }),
            json!([{ "name": "events", "direction": "out", "plane": "control",
                     "element": { "type": "any" } }]),
            json!({ "id": "e2", "source": "producer", "sourceHandle": "events",
                    "target": "egress", "targetHandle": "events", "type": "sequence" }),
        );
        let err = compile_err(&graph);
        assert_eq!(err.kind(), "channel_invalid", "got {err:?}");
    }

    /// `livekit` has no node-side consumer (browser-egress only) — a sink
    /// cannot drain it.
    #[test]
    fn validate_rejects_livekit_transport() {
        let graph = graph_with_sink(
            json!({ "name": "frames", "direction": "out", "plane": "data",
                    "element": { "type": "binary", "content_type": "image/jpeg" } }),
            json!([{ "name": "frames", "direction": "in", "plane": "data",
                     "element": { "type": "binary", "content_type": "image/jpeg" },
                     "transport": "livekit" }]),
            json!({ "id": "e2", "source": "producer", "sourceHandle": "frames",
                    "target": "egress", "targetHandle": "frames", "type": "sequence" }),
        );
        let err = compile_err(&graph);
        assert_eq!(err.kind(), "channel_invalid", "got {err:?}");
        assert!(err.to_string().contains("livekit"), "got {err}");
    }

    #[test]
    fn validate_rejects_outbound_edge() {
        let mut graph = control_fixture();
        graph.edges.push(
            serde_json::from_value(json!({
                "id": "e_bad", "source": "egress", "target": "end",
                "targetHandle": "in", "type": "sequence"
            }))
            .unwrap(),
        );
        let err = compile_err(&graph);
        assert_eq!(err.kind(), "validation", "got {err:?}");
        assert!(err.to_string().contains("outbound"), "got {err}");
    }
}
