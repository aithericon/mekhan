//! Synthetic event-log generators for the rehydration / replay axis.
//!
//! These produce [`petri_domain::PersistedEvent`] sequences that
//! [`petri_domain::project_marking`] can replay without any running engine.
//!
//! The chain log models the cheapest non-trivial shape an engine cold-wake
//! must replay: one token endlessly cycling through a small ring of places.
//! Each `TransitionFired` event removes the current token and adds a fresh one
//! at the next ring place, so projecting the whole log exercises the real
//! add/remove marking math (one `remove_token` + one `add_token` per event)
//! while the steady-state token total stays at exactly 1.

use petri_domain::{DomainEvent, PersistedEvent, PlaceId, Token};

/// Number of places in the ring the single token cycles through.
const RING_SIZE: usize = 4;

/// The ring place id for ring index `i` (e.g. `p0`..`p3`).
fn ring_place(i: usize) -> PlaceId {
    PlaceId::named(format!("p{}", i % RING_SIZE))
}

/// Build a hash-chained log of `n_events` events shaped as one token cycling
/// through a ring of [`RING_SIZE`] places (`p0`..`p3`).
///
/// - Event 0 is a [`DomainEvent::TokenCreated`] placing one fresh token at `p0`
///   (`previous_hash` = `None`).
/// - Events `1..n_events` are [`DomainEvent::TransitionFired`] events that each
///   consume the current token at its current ring place and produce a fresh
///   token at the next ring place.
/// - Every [`PersistedEvent`] chains to its predecessor via `previous_hash`.
///
/// Replaying the result through [`petri_domain::project_marking`] yields a
/// marking holding exactly one token (it just moved around the ring).
///
/// `n_events == 0` yields an empty log.
pub fn chain_log(n_events: usize) -> Vec<PersistedEvent> {
    let mut events = Vec::with_capacity(n_events);
    if n_events == 0 {
        return events;
    }

    // Event 0: create the single token at the first ring place.
    let mut cur_place = ring_place(0);
    let mut cur_token = Token::new_unit();
    let created = DomainEvent::TokenCreated {
        token: cur_token.clone(),
        place_id: cur_place.clone(),
        place_name: None,
        workflow_id: None,
        signal_key: None,
        dedup_id: None,
    };
    events.push(PersistedEvent::new(0, created, None));

    // Events 1..n_events: move the token one ring step per event.
    for seq in 1..n_events {
        let next_place = ring_place(seq);
        let next_token = Token::new_unit();

        let fired = DomainEvent::TransitionFired {
            transition_id: petri_domain::TransitionId::named(format!("t{}", seq)),
            transition_name: None,
            consumed_tokens: vec![(cur_place.clone(), cur_token.id.clone())],
            produced_tokens: vec![(next_place.clone(), next_token.clone())],
            read_tokens: vec![],
            process_step_started: None,
            process_step_completed: None,
        };

        let prev_hash = Some(events[seq - 1].hash.clone());
        events.push(PersistedEvent::new(seq as u64, fired, prev_hash));

        cur_place = next_place;
        cur_token = next_token;
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::project_marking;

    /// Total tokens across every place in a marking.
    fn total_tokens(marking: &petri_domain::Marking) -> usize {
        marking.tokens.values().map(|v| v.len()).sum()
    }

    #[test]
    fn chain_log_len_matches_request() {
        assert_eq!(chain_log(1).len(), 1);
        assert_eq!(chain_log(1000).len(), 1000);
    }

    #[test]
    fn chain_log_empty_is_empty() {
        assert!(chain_log(0).is_empty());
    }

    #[test]
    fn replay_holds_exactly_one_token() {
        let log = chain_log(1000);
        let marking = project_marking(&log);
        assert_eq!(
            total_tokens(&marking),
            1,
            "the single token should still be the only token after replay"
        );
    }

    #[test]
    fn single_event_replay_holds_one_token() {
        let marking = project_marking(&chain_log(1));
        assert_eq!(total_tokens(&marking), 1);
    }

    #[test]
    fn hash_chain_links_are_consistent() {
        let log = chain_log(1000);
        assert_eq!(log[0].previous_hash, None, "first event has no predecessor");
        for i in 1..log.len() {
            assert_eq!(
                log[i].previous_hash,
                Some(log[i - 1].hash.clone()),
                "event {i} must chain to event {}",
                i - 1
            );
        }
    }
}
