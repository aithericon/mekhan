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
// each one to its canonical engine layout.
//
// As of the multi-tenancy refactor the engine subject layout is
// `petri.{ws}.{net}.{category}.{suffix}` (workspace segment first, category
// AFTER the net). Service-side cross-workspace filters therefore wildcard both
// the workspace and net tokens: `petri.*.*.events.{suffix}`. NATS `*` matches
// exactly one dot-delimited token; workspace ids and net ids (`mekhan-{uuid}`,
// `default`) are single tokens.

/// `petri.` — petri root with separator, for `starts_with` subject dispatch
/// across every workspace/net.
pub const EVENTS_PREFIX_DOT: &str = "petri.";

/// `.bridge.` — bridge category segment with separators, for `contains`
/// subject dispatch (the category now follows `petri.{ws}.{net}.`).
pub const BRIDGE_PREFIX_DOT: &str = ".bridge.";

/// `petri.*.*.bridge.>` — every cross-net bridge transfer in any workspace.
pub const BRIDGE_ALL: &str = "petri.*.*.bridge.>";

/// `petri.*.*.events.>` — every domain event in any workspace/net.
///
/// This is the events-category half of the causality consumer's filter, kept
/// DISJOINT from [`BRIDGE_ALL`] so the two can coexist as `filter_subjects` on a
/// single JetStream consumer. The engine's `Subjects::EVENTS_ALL` is the broad
/// `petri.>` (used where a lone catch-all filter is wanted, e.g. the projections
/// framework); pairing *that* with `BRIDGE_ALL` is rejected by JetStream
/// (error 10138, "subject filters cannot overlap") since `petri.>` subsumes
/// `petri.*.*.bridge.>`. Scoping to the `events` category restores disjointness
/// and also keeps `signal`/other categories (which share the stream) out of the
/// projector, which only understands events + bridge.
pub const EVENTS_CATEGORY_ALL: &str = "petri.*.*.events.>";

/// `petri.*.*.events.net.>` — net lifecycle events (created/completed/cancelled)
/// for every net in every workspace. NATS `*` matches an entire dot-delimited
/// token; workspace ids and net ids (`mekhan-{uuid}`) are single tokens.
pub const NET_LIFECYCLE_EVENTS_FILTER: &str = "petri.*.*.events.net.>";

/// `petri.*.*.events.effect.completed` — every net's `EffectCompleted` events.
pub const EFFECT_COMPLETED_EVENTS_FILTER: &str = "petri.*.*.events.effect.completed";

/// `petri.*.*.events.effect.failed` — every net's `EffectFailed` events.
pub const EFFECT_FAILED_EVENTS_FILTER: &str = "petri.*.*.events.effect.failed";

/// `petri.*.*.events.token.created` — every net's `TokenCreated` events.
pub const TOKEN_CREATED_EVENTS_FILTER: &str = "petri.*.*.events.token.created";

/// `petri.*.*.events.transition.fired` — every net's `TransitionFired` events.
pub const TRANSITION_FIRED_EVENTS_FILTER: &str = "petri.*.*.events.transition.fired";

/// `petri.*.{net_id}.events.>` — every event for one net, across whichever
/// workspace owns it. Net ids are globally unique, so the `*` ws wildcard is
/// safe for service-side purge/stream-by-net (the caller has the net id but
/// not always the workspace).
pub fn net_events_filter(net_id: &str) -> String {
    format!(
        "{}.*.{net_id}.{}.>",
        Subjects::PETRI_ROOT,
        Subjects::EVENTS_CATEGORY
    )
}

/// `petri.*.{net_id}.signal.>` — every external signal targeting one net,
/// across whichever workspace owns it.
pub fn net_signals_filter(net_id: &str) -> String {
    format!(
        "{}.*.{net_id}.{}.>",
        Subjects::PETRI_ROOT,
        Subjects::SIGNAL_CATEGORY
    )
}

// ==================== Service-owned streams/subjects ====================

/// Human task request stream. Mekhan-owned: the engine publishes requests on
/// `Subjects::human_request` subjects, mekhan creates the stream and consumes.
pub const STREAM_HUMAN_REQUESTS: &str = "HUMAN_REQUESTS";

/// `human.*.request.>` — every human task request, across every workspace.
/// Layout is now `human.{ws}.request.{net}.{place}` (workspace segment after
/// the `human` root, category after the ws), so the cross-workspace filter
/// wildcards the ws token.
pub const HUMAN_REQUEST_ALL: &str = "human.*.request.>";

/// `human.*.cancel.>` — every engine-initiated human task cancellation, across
/// every workspace.
pub const HUMAN_CANCEL_ALL: &str = "human.*.cancel.>";

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

    /// Pin the allocation-free literals to the canonical engine layout so a
    /// rename on the engine side fails here instead of silently desyncing.
    ///
    /// Layout is `petri.{ws}.{net}.{category}.{suffix}`; the cross-workspace
    /// service filters wildcard the ws + net tokens with `*`, and the category
    /// + suffix tokens stay engine-canonical.
    #[test]
    fn derived_consts_match_engine_prefixes() {
        assert_eq!(EVENTS_PREFIX_DOT, format!("{}.", Subjects::PETRI_ROOT));
        assert_eq!(
            BRIDGE_PREFIX_DOT,
            format!(".{}.", Subjects::BRIDGE_CATEGORY)
        );
        assert_eq!(
            BRIDGE_ALL,
            format!(
                "{}.*.*.{}.>",
                Subjects::PETRI_ROOT,
                Subjects::BRIDGE_CATEGORY
            )
        );
        assert_eq!(
            EVENTS_CATEGORY_ALL,
            format!(
                "{}.*.*.{}.>",
                Subjects::PETRI_ROOT,
                Subjects::EVENTS_CATEGORY
            )
        );
        // The causality consumer pairs these two as `filter_subjects`; JetStream
        // rejects overlapping filters, so they MUST stay category-disjoint. (The
        // historical bug paired `EVENTS_ALL` = `petri.>` with `BRIDGE_ALL`, which
        // overlaps and silently killed the projection.)
        assert_ne!(EVENTS_CATEGORY_ALL, BRIDGE_ALL);
        assert!(!EVENTS_CATEGORY_ALL.contains(Subjects::BRIDGE_CATEGORY));
        assert!(!BRIDGE_ALL.contains(Subjects::EVENTS_CATEGORY));
        assert_eq!(
            NET_LIFECYCLE_EVENTS_FILTER,
            format!(
                "{}.*.*.{}.net.>",
                Subjects::PETRI_ROOT,
                Subjects::EVENTS_CATEGORY
            )
        );
        assert_eq!(
            EFFECT_COMPLETED_EVENTS_FILTER,
            format!(
                "{}.*.*.{}.effect.completed",
                Subjects::PETRI_ROOT,
                Subjects::EVENTS_CATEGORY
            )
        );
        assert_eq!(
            EFFECT_FAILED_EVENTS_FILTER,
            format!(
                "{}.*.*.{}.effect.failed",
                Subjects::PETRI_ROOT,
                Subjects::EVENTS_CATEGORY
            )
        );
        assert_eq!(
            TOKEN_CREATED_EVENTS_FILTER,
            format!(
                "{}.*.*.{}.token.created",
                Subjects::PETRI_ROOT,
                Subjects::EVENTS_CATEGORY
            )
        );
        assert_eq!(
            TRANSITION_FIRED_EVENTS_FILTER,
            format!(
                "{}.*.*.{}.transition.fired",
                Subjects::PETRI_ROOT,
                Subjects::EVENTS_CATEGORY
            )
        );
        assert_eq!(
            HUMAN_REQUEST_ALL,
            format!(
                "{}.*.{}.>",
                Subjects::HUMAN_ROOT,
                Subjects::HUMAN_REQUEST_CATEGORY
            )
        );
        assert_eq!(
            HUMAN_CANCEL_ALL,
            format!(
                "{}.*.{}.>",
                Subjects::HUMAN_ROOT,
                Subjects::HUMAN_CANCEL_CATEGORY
            )
        );
    }

    #[test]
    fn per_net_filters() {
        assert_eq!(net_events_filter("net-a"), "petri.*.net-a.events.>");
        assert_eq!(net_signals_filter("net-a"), "petri.*.net-a.signal.>");
    }
}
