//! Per-net negative-binding memo — skip the repeated binding search for a
//! transition that is waiting (has no valid binding) until one of its input
//! places actually changes.
//!
//! `find_valid_binding` (see [`crate::binding`]) can cost up to `m^k` guard
//! evaluations for a guarded multi-input transition. The equi-join index
//! (`crate::join_index`) collapses that for `==`-correlated guards, but a guard
//! that is *not* a simple equality — the chief example being the presence-pool
//! grant `satisfies(claim.requirements, unit.caps)` — still pays the full
//! cross-product. The live engine re-runs the whole eval loop on *every*
//! incoming event, so a waiting join re-pays that cost on every tick even when
//! none of its inputs moved. This couples its cost to the event rate
//! (`docs/engine/scalability.md` §4 P1 increment 2).
//!
//! The memo breaks that coupling: once `select_next_transition` proves a
//! transition has no valid binding, it records that. On subsequent ticks the
//! transition is skipped — without re-running the search — until a token
//! arrives at, leaves, or changes on one of its input places (or its guard /
//! the net structure changes).
//!
//! ## Why this is sound (selection-equivalent ⇒ replay-deterministic)
//!
//! The memo is **negative only**: it never caches a *positive* binding (those
//! feed selection ordering and consume real tokens, so they must always be
//! recomputed fresh). It only ever *suppresses* a transition that
//! `find_valid_binding` would return `None` for. So the set of transitions that
//! produce a candidate binding — and therefore the one the selector picks — is
//! identical with or without the memo. The only way the memo could change a
//! decision is a *stale* "no binding" entry for a transition that has since
//! become enabled; that is prevented because the memo is reconciled from the
//! **same event delta** that advances the marking cache, so it can never lag
//! the marking the loop evaluates against.
//!
//! A "no valid binding" verdict for transition `T` is a pure function of the
//! tokens at `T`'s input places (all of `T`'s read/count/consume arcs are input
//! arcs), its guard, and the schema registry. Hence it is invalidated exactly
//! by: a token add/remove/update at one of `T`'s input places; a change to
//! `T`'s guard (`TransitionScriptUpdated`); or a net-structure change
//! (`NetInitialized` / full marking rebuild). Over-invalidation (e.g. on a pure
//! token *removal*, which can never newly enable a transition) is harmless — it
//! only forgoes a cache hit, never admits a wrong one.

use std::collections::{HashMap, HashSet};

use petri_domain::{DomainEvent, PetriNet, PlaceId, TransitionId};

/// Per-net negative-binding memo. Lives on the service alongside the marking
/// cache and is only ever touched under the service's `eval_lock`, so its
/// internal locking sees no real contention.
#[derive(Default)]
pub(crate) struct BindingMemo {
    /// Transitions proven to have NO valid binding at the current marking.
    no_binding: HashSet<TransitionId>,
    /// `place → transitions with an input arc on it` (consumers). Rebuilt from
    /// topology on a full reset; used to invalidate the right entries when a
    /// place's token set changes.
    consumers: HashMap<PlaceId, Vec<TransitionId>>,
    /// Whether `consumers` has been built from a topology yet.
    built: bool,
}

impl BindingMemo {
    /// Rebuild the place→consumers reverse index from a topology and clear all
    /// negative entries (guards/arcs may have changed wholesale).
    pub(crate) fn rebuild_index(&mut self, net: &PetriNet) {
        let mut consumers: HashMap<PlaceId, Vec<TransitionId>> = HashMap::new();
        for transition in net.transitions.values() {
            for arc in net.input_arcs(&transition.id) {
                consumers
                    .entry(arc.place_id.clone())
                    .or_default()
                    .push(transition.id.clone());
            }
        }
        self.consumers = consumers;
        self.no_binding.clear();
        self.built = true;
    }

    /// Drop every negative entry but keep the reverse index (used when the
    /// topology is absent on a rebuild).
    pub(crate) fn clear_entries(&mut self) {
        self.no_binding.clear();
    }

    /// Has `transition` been proven to have no valid binding (and not
    /// invalidated since)? The selector reads the set via [`Self::snapshot`];
    /// this point query is used by the unit tests.
    #[cfg(test)]
    pub(crate) fn is_known_empty(&self, transition: &TransitionId) -> bool {
        self.no_binding.contains(transition)
    }

    /// Record that `transition` has no valid binding at the current marking.
    pub(crate) fn mark_empty(&mut self, transition: TransitionId) {
        self.no_binding.insert(transition);
    }

    /// A snapshot of the currently-known-empty set, for a lock-free scan in the
    /// selector.
    pub(crate) fn snapshot(&self) -> HashSet<TransitionId> {
        self.no_binding.clone()
    }

    /// Whether the reverse index has been built from a topology yet. The caller
    /// uses this to decide whether it must fetch (clone) the net before
    /// reconciling — so the common token-event path never clones the net.
    pub(crate) fn is_index_built(&self) -> bool {
        self.built
    }

    /// Invalidate every transition that consumes from `place`.
    fn invalidate_place(&mut self, place: &PlaceId) {
        if let Some(transitions) = self.consumers.get(place) {
            for t in transitions {
                self.no_binding.remove(t);
            }
        }
    }

    /// Reconcile against the events that just advanced the marking, in one pass.
    ///
    /// `net` (the current topology snapshot) is required only when the reverse
    /// index must be (re)built — i.e. on the first call or when the delta
    /// carries a `NetInitialized` (structural change). The caller passes `None`
    /// on the common token-event path so the net is never cloned there.
    pub(crate) fn apply_events<'a, I>(&mut self, net: Option<&PetriNet>, events: I)
    where
        I: Iterator<Item = &'a DomainEvent>,
    {
        if !self.built {
            match net {
                Some(n) => self.rebuild_index(n),
                // No topology yet — nothing consumes anything; entries (none
                // yet) stay clear. The next rebuild with a net seeds the index.
                None => self.clear_entries(),
            }
        }

        for event in events {
            match event {
                // A net (re)load changes places/arcs/guards wholesale: rebuild
                // the reverse index and clear every entry. Later token events in
                // the same delta then invalidate against the fresh (empty) set
                // harmlessly.
                DomainEvent::NetInitialized { .. } => match net {
                    Some(n) => self.rebuild_index(n),
                    None => self.clear_entries(),
                },
                // A guard/script change can newly enable exactly this
                // transition — drop only its entry; the reverse index (arcs)
                // is unchanged by a script hot-reload.
                DomainEvent::TransitionScriptUpdated { transition_id, .. } => {
                    self.no_binding.remove(transition_id);
                }
                other => {
                    for place in touched_places(other) {
                        self.invalidate_place(place);
                    }
                }
            }
        }
    }
}

/// Places whose token multiset is changed by `event` (added, removed, or
/// updated). A superset is safe; this returns exactly the places `apply_event_to_marking`
/// mutates, so it stays in lockstep with the marking.
fn touched_places(event: &DomainEvent) -> Vec<&PlaceId> {
    match event {
        DomainEvent::TokenCreated { place_id, .. }
        | DomainEvent::TokenConsumed { place_id, .. }
        | DomainEvent::TokenRemoved { place_id, .. }
        | DomainEvent::TokenUpdated { place_id, .. } => vec![place_id],
        DomainEvent::TransitionFired {
            consumed_tokens,
            produced_tokens,
            ..
        }
        | DomainEvent::EffectCompleted {
            consumed_tokens,
            produced_tokens,
            ..
        }
        | DomainEvent::TransitionSkipped {
            consumed_tokens,
            produced_tokens,
            ..
        } => consumed_tokens
            .iter()
            .map(|(p, _)| p)
            .chain(produced_tokens.iter().map(|(p, _)| p))
            .collect(),
        DomainEvent::EffectFailed {
            consumed_tokens,
            produced_tokens,
            tokens_consumed,
            ..
        } => {
            if *tokens_consumed {
                consumed_tokens
                    .iter()
                    .map(|(p, _)| p)
                    .chain(produced_tokens.iter().map(|(p, _)| p))
                    .collect()
            } else {
                Vec::new()
            }
        }
        // Audit-only / lifecycle / bridge-out events never change the local
        // marking, so they never invalidate a binding verdict.
        DomainEvent::NetInitialized { .. }
        | DomainEvent::TransitionScriptUpdated { .. }
        | DomainEvent::ErrorOccurred { .. }
        | DomainEvent::TokenBridgedOut { .. }
        | DomainEvent::NetCreated { .. }
        | DomainEvent::NetCompleted { .. }
        | DomainEvent::NetCancelled { .. }
        | DomainEvent::NetFailed { .. }
        | DomainEvent::PreDispatchEvaluated { .. }
        | DomainEvent::PreDispatchRejected { .. }
        | DomainEvent::PreDispatchDeferred { .. } => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::{
        Arc as PetriArc, PetriNet, Place, PlaceId, Token, TokenColor, TokenId, Transition,
        TransitionId,
    };

    fn place(name: &str) -> PlaceId {
        PlaceId::named(name)
    }

    /// Two-input join `t_join` consuming from `a` and `b`, plus an unrelated
    /// single-input `t_other` consuming from `c`.
    fn join_net() -> PetriNet {
        let mut net = PetriNet::new();
        net.add_place(Place::internal("a"));
        net.add_place(Place::internal("b"));
        net.add_place(Place::internal("c"));
        let tj = TransitionId::named("t_join");
        let to = TransitionId::named("t_other");
        net.add_transition(
            Transition::new("t_join", "#{}")
                .with_input_port(petri_domain::Port::new("a"))
                .with_input_port(petri_domain::Port::new("b"))
                .with_guard("a.k == b.k"),
        );
        net.add_transition(
            Transition::new("t_other", "#{}").with_input_port(petri_domain::Port::new("c")),
        );
        net.add_arc(PetriArc::input(place("a"), tj.clone(), "a"));
        net.add_arc(PetriArc::input(place("b"), tj, "b"));
        net.add_arc(PetriArc::input(place("c"), to, "c"));
        net
    }

    fn created(place_name: &str) -> DomainEvent {
        DomainEvent::TokenCreated {
            token: Token::new(TokenColor::Unit),
            place_id: place(place_name),
            place_name: None,
            workflow_id: None,
            signal_key: None,
            dedup_id: None,
        }
    }

    fn fired(consumed: &[&str], produced: &[&str]) -> DomainEvent {
        DomainEvent::TransitionFired {
            transition_id: TransitionId::named("x"),
            transition_name: None,
            consumed_tokens: consumed
                .iter()
                .map(|p| (place(p), TokenId::new()))
                .collect(),
            produced_tokens: produced
                .iter()
                .map(|p| (place(p), Token::new(TokenColor::Unit)))
                .collect(),
            read_tokens: vec![],
            process_step_started: None,
            process_step_completed: None,
        }
    }

    #[test]
    fn empty_entry_survives_unrelated_change() {
        let net = join_net();
        let mut memo = BindingMemo::default();
        memo.apply_events(Some(&net), std::iter::empty()); // build index
        memo.mark_empty(TransitionId::named("t_join"));

        // A token lands at `c` (input of t_other, NOT t_join).
        memo.apply_events(Some(&net), [created("c")].iter());

        assert!(
            memo.is_known_empty(&TransitionId::named("t_join")),
            "t_join's verdict must survive a change to an unrelated place"
        );
    }

    #[test]
    fn token_at_input_place_invalidates() {
        let net = join_net();
        let mut memo = BindingMemo::default();
        memo.apply_events(Some(&net), std::iter::empty());
        memo.mark_empty(TransitionId::named("t_join"));

        // A token lands at `a`, an input of t_join.
        memo.apply_events(Some(&net), [created("a")].iter());

        assert!(
            !memo.is_known_empty(&TransitionId::named("t_join")),
            "a token at an input place must invalidate the verdict"
        );
    }

    #[test]
    fn firing_consumed_and_produced_places_both_invalidate() {
        let net = join_net();
        let mut memo = BindingMemo::default();
        memo.apply_events(Some(&net), std::iter::empty());

        // Producing into `b` invalidates t_join (b is its input).
        memo.mark_empty(TransitionId::named("t_join"));
        memo.apply_events(Some(&net), [fired(&["c"], &["b"])].iter());
        assert!(!memo.is_known_empty(&TransitionId::named("t_join")));

        // Consuming from `a` invalidates t_join too.
        memo.mark_empty(TransitionId::named("t_join"));
        memo.apply_events(Some(&net), [fired(&["a"], &["out"])].iter());
        assert!(!memo.is_known_empty(&TransitionId::named("t_join")));
    }

    #[test]
    fn script_update_invalidates_only_that_transition() {
        let net = join_net();
        let mut memo = BindingMemo::default();
        memo.apply_events(Some(&net), std::iter::empty());
        memo.mark_empty(TransitionId::named("t_join"));
        memo.mark_empty(TransitionId::named("t_other"));

        memo.apply_events(
            Some(&net),
            [DomainEvent::TransitionScriptUpdated {
                transition_id: TransitionId::named("t_join"),
                script: "#{}".to_string(),
                guard: Some("true".to_string()),
            }]
            .iter(),
        );

        assert!(!memo.is_known_empty(&TransitionId::named("t_join")));
        assert!(
            memo.is_known_empty(&TransitionId::named("t_other")),
            "a guard change must not touch an unrelated transition"
        );
    }

    #[test]
    fn net_initialized_clears_all_entries() {
        let net = join_net();
        let mut memo = BindingMemo::default();
        memo.apply_events(Some(&net), std::iter::empty());
        memo.mark_empty(TransitionId::named("t_join"));
        memo.mark_empty(TransitionId::named("t_other"));

        memo.apply_events(
            Some(&net),
            [DomainEvent::NetInitialized { net: net.clone() }].iter(),
        );

        assert!(!memo.is_known_empty(&TransitionId::named("t_join")));
        assert!(!memo.is_known_empty(&TransitionId::named("t_other")));
    }

    #[test]
    fn audit_only_event_does_not_invalidate() {
        let net = join_net();
        let mut memo = BindingMemo::default();
        memo.apply_events(Some(&net), std::iter::empty());
        memo.mark_empty(TransitionId::named("t_join"));

        // A lifecycle event never changes the local marking.
        memo.apply_events(
            Some(&net),
            [DomainEvent::NetCancelled {
                net_id: "test".into(),
                reason: None,
                cancelled_by: None,
            }]
            .iter(),
        );

        assert!(memo.is_known_empty(&TransitionId::named("t_join")));
    }

    #[test]
    fn effect_failed_without_consumption_does_not_invalidate() {
        let net = join_net();
        let mut memo = BindingMemo::default();
        memo.apply_events(Some(&net), std::iter::empty());
        memo.mark_empty(TransitionId::named("t_join"));

        memo.apply_events(
            Some(&net),
            [DomainEvent::EffectFailed {
                transition_id: TransitionId::named("x"),
                transition_name: None,
                consumed_tokens: vec![(place("a"), TokenId::new())],
                produced_tokens: vec![],
                effect_handler_id: "h".to_string(),
                error_message: "boom".to_string(),
                tokens_consumed: false,
                input_data: None,
                retryable: false,
            }]
            .iter(),
        );

        assert!(
            memo.is_known_empty(&TransitionId::named("t_join")),
            "an audit-only EffectFailed (tokens_consumed=false) changes no marking"
        );
    }
}
