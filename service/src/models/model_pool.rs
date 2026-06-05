//! Model-pool P1 (docs/28 + docs/29) — DTOs + the loaded-state machine.
//!
//! This is a CONTROL/PROJECTION seam only: inference bypasses the engine Petri
//! net + the presence net, and P1 adds NO NATS subjects. The `model_states`
//! table is the operator-curated lifecycle projection; the loaded-set read
//! AND-gates `state == Loaded` against a live runner interface catalog that
//! advertises the `model_id` (the live half — see [`crate::handlers::model_pool`]).
//!
//! The state machine (`approved → loading → loaded → draining → unloaded`) is
//! enforced HERE in Rust ([`ModelState::legal_transitions`]), NOT by a DB CHECK:
//! a `POST .../transition` over an illegal edge returns 409.
//!
//! `ApprovedModelConfig` is RE-EXPORTED from `aithericon_resources` (where it is
//! defined so the `model_registry` descriptor schema picks it up for free) — one
//! shape, no duplication, no cyclic dep.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

pub use aithericon_resources::types::ApprovedModelConfig;

/// The operator-curated lifecycle position of a model in the pool. Persisted as
/// the free-text `model_states.state` column; validated against this enum on
/// every read/write (no DB CHECK).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelState {
    /// In the registry's approved SET, no replica requested yet.
    Approved,
    /// A replica load was requested; node-agent is fetching/warming.
    Loading,
    /// Operator says it's loaded. (The PUBLIC "available" flag is an AND of this
    /// with a live runner advertising the model_id — see the projection.)
    Loaded,
    /// Operator requested teardown; existing sessions drain.
    Draining,
    /// Fully torn down; no replicas.
    Unloaded,
}

impl ModelState {
    /// The wire string for this state (matches the serde `snake_case` rename).
    pub fn as_str(self) -> &'static str {
        match self {
            ModelState::Approved => "approved",
            ModelState::Loading => "loading",
            ModelState::Loaded => "loaded",
            ModelState::Draining => "draining",
            ModelState::Unloaded => "unloaded",
        }
    }

    /// Parse a stored/wire string back into the enum. `None` for an unknown
    /// value (a row written outside this code path).
    pub fn parse(s: &str) -> Option<ModelState> {
        match s {
            "approved" => Some(ModelState::Approved),
            "loading" => Some(ModelState::Loading),
            "loaded" => Some(ModelState::Loaded),
            "draining" => Some(ModelState::Draining),
            "unloaded" => Some(ModelState::Unloaded),
            _ => None,
        }
    }

    /// The states this state may legally transition TO. The whole lifecycle is:
    ///
    /// ```text
    /// approved → loading → loaded → draining → unloaded
    ///                ↑__________________________|   (re-load after teardown)
    /// ```
    ///
    /// - `approved` may only begin `loading` (NOT jump straight to `loaded`).
    /// - `loading` resolves to `loaded`, or aborts back to `unloaded`.
    /// - `loaded` may begin `draining`.
    /// - `draining` completes to `unloaded`.
    /// - `unloaded` may re-enter the cycle at `loading` (operator re-loads an
    ///   approved-but-torn-down model).
    pub fn legal_transitions(self) -> &'static [ModelState] {
        use ModelState::*;
        match self {
            Approved => &[Loading],
            Loading => &[Loaded, Unloaded],
            Loaded => &[Draining],
            Draining => &[Unloaded],
            Unloaded => &[Loading],
        }
    }

    /// Whether a transition `self → target` is legal.
    pub fn can_transition_to(self, target: ModelState) -> bool {
        self.legal_transitions().contains(&target)
    }
}

/// Request body for `POST /api/v1/models/{model_id}/transition` — the operator
/// state-machine step. Validated against [`ModelState::legal_transitions`]; an
/// illegal edge → 409.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TransitionRequest {
    /// The state to move the model to.
    pub target: ModelState,
    /// Optional operator note recorded with the transition.
    #[serde(default)]
    pub note: Option<String>,
}

/// One row of the loaded-set projection (`GET /api/v1/models` and
/// `GET /api/v1/models/{model_id}`).
///
/// `state` is the operator-curated `model_states` position; `available` is the
/// AND-gate — `true` only when `state == Loaded` AND at least one LIVE runner's
/// interface catalog advertises `model_id`. `serving_runners` is the count of
/// live runners advertising it (the live half), surfaced so an operator can see
/// a `loaded`-but-unserved model.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelSetView {
    /// The model id (router routes on this).
    pub model_id: String,
    /// The operator-curated lifecycle state.
    pub state: ModelState,
    /// For a LoRA model, the base model id it layers on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Operator-tracked replica count (rides the Nomad job-template; P1 manual).
    pub replicas: i32,
    /// AND-gate: `state == Loaded` AND a live runner advertises `model_id`. This
    /// is the flag the editor model picker filters on.
    pub available: bool,
    /// Count of LIVE runners whose interface catalog advertises `model_id`.
    pub serving_runners: u32,
    /// Optional operator note from the last transition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A `model_states` DB row (column order mirrors the migration). Mapped to
/// [`ModelSetView`] by the handler after the live-runner AND-gate is computed.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ModelStateRow {
    pub workspace_id: Uuid,
    pub registry_resource_id: Option<Uuid>,
    pub model_id: String,
    /// Free-text on the wire; parsed through [`ModelState::parse`].
    pub state: String,
    pub base: Option<String>,
    pub replicas: i32,
    pub note: Option<String>,
}

impl ModelStateRow {
    /// Build the projection view, given the count of LIVE runners advertising
    /// this model_id (the live half of the AND-gate). `state` is parsed; an
    /// unparseable stored value defaults to `Unloaded` (fail-closed: never offer
    /// a model whose state we can't read).
    pub fn into_view(self, serving_runners: u32) -> ModelSetView {
        let state = ModelState::parse(&self.state).unwrap_or(ModelState::Unloaded);
        let available = state == ModelState::Loaded && serving_runners > 0;
        ModelSetView {
            model_id: self.model_id,
            state,
            base: self.base,
            replicas: self.replicas,
            available,
            serving_runners,
            note: self.note,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_to_loading_is_legal() {
        assert!(ModelState::Approved.can_transition_to(ModelState::Loading));
    }

    #[test]
    fn approved_to_loaded_is_illegal() {
        // Must go through `loading` first — no skipping straight to loaded.
        assert!(!ModelState::Approved.can_transition_to(ModelState::Loaded));
    }

    #[test]
    fn loaded_draining_unloaded_chain_is_legal() {
        assert!(ModelState::Loaded.can_transition_to(ModelState::Draining));
        assert!(ModelState::Draining.can_transition_to(ModelState::Unloaded));
    }

    #[test]
    fn unloaded_may_reload() {
        assert!(ModelState::Unloaded.can_transition_to(ModelState::Loading));
    }

    #[test]
    fn illegal_edges_rejected() {
        // A representative sweep of edges that must NOT be allowed.
        assert!(!ModelState::Approved.can_transition_to(ModelState::Draining));
        assert!(!ModelState::Approved.can_transition_to(ModelState::Unloaded));
        assert!(!ModelState::Loading.can_transition_to(ModelState::Draining));
        assert!(!ModelState::Loaded.can_transition_to(ModelState::Loading));
        assert!(!ModelState::Loaded.can_transition_to(ModelState::Unloaded));
        assert!(!ModelState::Draining.can_transition_to(ModelState::Loaded));
        assert!(!ModelState::Unloaded.can_transition_to(ModelState::Loaded));
        // No self-loops in the lifecycle.
        for s in [
            ModelState::Approved,
            ModelState::Loading,
            ModelState::Loaded,
            ModelState::Draining,
            ModelState::Unloaded,
        ] {
            assert!(!s.can_transition_to(s), "{s:?} should not self-loop");
        }
    }

    #[test]
    fn state_str_roundtrips() {
        for s in [
            ModelState::Approved,
            ModelState::Loading,
            ModelState::Loaded,
            ModelState::Draining,
            ModelState::Unloaded,
        ] {
            assert_eq!(ModelState::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn projection_and_gate() {
        // loaded + a serving runner → available
        let row = ModelStateRow {
            workspace_id: Uuid::nil(),
            registry_resource_id: None,
            model_id: "llama3".into(),
            state: "loaded".into(),
            base: None,
            replicas: 1,
            note: None,
        };
        let view = row.clone().into_view(1);
        assert!(view.available);
        assert_eq!(view.serving_runners, 1);

        // loaded but NO serving runner → NOT available (the AND-gate)
        let view = row.clone().into_view(0);
        assert!(!view.available);

        // approved (with a serving runner, somehow) → NOT available
        let mut approved = row.clone();
        approved.state = "approved".into();
        assert!(!approved.into_view(1).available);

        // unparseable stored state fails closed to unloaded → NOT available
        let mut bad = row;
        bad.state = "weird".into();
        let view = bad.into_view(5);
        assert_eq!(view.state, ModelState::Unloaded);
        assert!(!view.available);
    }
}
