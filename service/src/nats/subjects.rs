//! NATS subject and stream names the service touches.
//!
//! Engine-owned names come from the canonical [`petri_api_types::subjects`]
//! constants (re-exported below) so the service can't drift from what the
//! engine publishes. Service-owned streams/subjects that the engine doesn't
//! define get their consts here. (The `EXECUTOR_*` mirrors live in
//! `crate::streams` and are intentionally not duplicated here.)

pub use petri_api_types::subjects::Subjects;

// ==================== Engine-owned, derived filters ====================
//
// Literal consts (instead of `format!` over the engine prefixes) so they can
// be used in per-message hot paths without allocating; the tests below pin
// each one to its canonical engine prefix.

/// `petri.events.` — events prefix with separator, for `starts_with` subject
/// dispatch.
pub const EVENTS_PREFIX_DOT: &str = "petri.events.";

/// `petri.bridge.` — bridge prefix with separator, for `starts_with` subject
/// dispatch.
pub const BRIDGE_PREFIX_DOT: &str = "petri.bridge.";

/// `petri.bridge.>` — every cross-net bridge transfer.
pub const BRIDGE_ALL: &str = "petri.bridge.>";

/// `petri.events.*.net.>` — net lifecycle events (created/completed/cancelled)
/// for every net. NATS `*` matches an entire dot-delimited token; net IDs like
/// `mekhan-{uuid}` are single tokens (no dots), so `*` matches them.
pub const NET_LIFECYCLE_EVENTS_FILTER: &str = "petri.events.*.net.>";

/// `petri.events.*.effect.completed` — every net's `EffectCompleted` events.
pub const EFFECT_COMPLETED_EVENTS_FILTER: &str = "petri.events.*.effect.completed";

/// `petri.events.*.effect.failed` — every net's `EffectFailed` events.
pub const EFFECT_FAILED_EVENTS_FILTER: &str = "petri.events.*.effect.failed";

/// `petri.events.*.token.created` — every net's `TokenCreated` events.
pub const TOKEN_CREATED_EVENTS_FILTER: &str = "petri.events.*.token.created";

/// `petri.events.*.transition.fired` — every net's `TransitionFired` events.
pub const TRANSITION_FIRED_EVENTS_FILTER: &str = "petri.events.*.transition.fired";

/// `petri.events.{net_id}.>` — every event for one net.
pub fn net_events_filter(net_id: &str) -> String {
    format!("{}.{net_id}.>", Subjects::EVENTS_PREFIX)
}

/// `petri.signal.{net_id}.>` — every external signal targeting one net.
pub fn net_signals_filter(net_id: &str) -> String {
    Subjects::signal_inbox_filter(net_id)
}

// ==================== Service-owned streams/subjects ====================

/// Human task request stream. Mekhan-owned: the engine publishes requests on
/// `Subjects::human_request` subjects, mekhan creates the stream and consumes.
pub const STREAM_HUMAN_REQUESTS: &str = "HUMAN_REQUESTS";

/// `human.request.>` — every human task request.
pub const HUMAN_REQUEST_ALL: &str = "human.request.>";

/// `human.cancel.>` — every engine-initiated human task cancellation.
pub const HUMAN_CANCEL_ALL: &str = "human.cancel.>";

/// Dead-letter stream for messages a consumer couldn't process (see
/// `MekhanNats::ensure_silent_drops_stream`).
pub const STREAM_SILENT_DROPS: &str = "MEKHAN_SILENT_DROPS";

/// `mekhan.silent_drops.>` — every silent-drop forensic record.
pub const SILENT_DROPS_ALL: &str = "mekhan.silent_drops.>";

/// Inference-metering audit ledger stream (model-pool P5, docs/29 §7').
pub const STREAM_INFERENCE_METERING: &str = "INFERENCE_METERING";

/// `inference.metering.>` — one record per routed inference request.
pub const INFERENCE_METERING_ALL: &str = "inference.metering.>";

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the allocation-free literals to the canonical engine constants so
    /// a rename on the engine side fails here instead of silently desyncing.
    #[test]
    fn derived_consts_match_engine_prefixes() {
        assert_eq!(EVENTS_PREFIX_DOT, format!("{}.", Subjects::EVENTS_PREFIX));
        assert_eq!(BRIDGE_PREFIX_DOT, format!("{}.", Subjects::BRIDGE_PREFIX));
        assert_eq!(BRIDGE_ALL, format!("{}.>", Subjects::BRIDGE_PREFIX));
        assert_eq!(
            NET_LIFECYCLE_EVENTS_FILTER,
            format!("{}.*.net.>", Subjects::EVENTS_PREFIX)
        );
        // The engine's net-less subjects are `petri.events.{suffix}`; the
        // per-net filters insert the single-token net-id wildcard after the
        // prefix, so the suffix tokens stay engine-canonical.
        assert_eq!(
            EFFECT_COMPLETED_EVENTS_FILTER,
            Subjects::EVENT_EFFECT_COMPLETED.replacen(
                &format!("{}.", Subjects::EVENTS_PREFIX),
                &format!("{}.*.", Subjects::EVENTS_PREFIX),
                1
            )
        );
        assert_eq!(
            EFFECT_FAILED_EVENTS_FILTER,
            Subjects::EVENT_EFFECT_FAILED.replacen(
                &format!("{}.", Subjects::EVENTS_PREFIX),
                &format!("{}.*.", Subjects::EVENTS_PREFIX),
                1
            )
        );
        assert_eq!(
            TOKEN_CREATED_EVENTS_FILTER,
            Subjects::EVENT_TOKEN_CREATED.replacen(
                &format!("{}.", Subjects::EVENTS_PREFIX),
                &format!("{}.*.", Subjects::EVENTS_PREFIX),
                1
            )
        );
        assert_eq!(
            TRANSITION_FIRED_EVENTS_FILTER,
            Subjects::EVENT_TRANSITION_FIRED.replacen(
                &format!("{}.", Subjects::EVENTS_PREFIX),
                &format!("{}.*.", Subjects::EVENTS_PREFIX),
                1
            )
        );
        assert_eq!(
            HUMAN_REQUEST_ALL,
            format!("{}.>", Subjects::HUMAN_REQUEST_PREFIX)
        );
        assert_eq!(
            HUMAN_CANCEL_ALL,
            format!("{}.>", Subjects::HUMAN_CANCEL_PREFIX)
        );
    }

    #[test]
    fn per_net_filters() {
        assert_eq!(net_events_filter("net-a"), "petri.events.net-a.>");
        assert_eq!(net_signals_filter("net-a"), "petri.signal.net-a.>");
    }
}
