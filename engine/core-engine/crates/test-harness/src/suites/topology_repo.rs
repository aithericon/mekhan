//! Generic test suite for TopologyRepository implementations.
//!
//! This module provides test functions that can be used with rstest to validate
//! any TopologyRepository implementation.
//!
//! # Usage with rstest
//!
//! ```ignore
//! use rstest::rstest;
//! use petri_test_harness::suites::topology_repo::*;
//!
//! #[rstest]
//! fn test_set_and_get() {
//!     assert_set_and_get(&MemoryTopologyStore::new());
//! }
//! ```

use petri_application::TopologyRepository;
use petri_domain::{Arc, PetriNet, Place, Port, Transition};

/// Create a simple test PetriNet with one transition.
fn test_net() -> PetriNet {
    let mut net = PetriNet::new();

    let place_a = Place::internal("A");
    let place_b = Place::internal("B");
    let place_a_id = place_a.id.clone();
    let place_b_id = place_b.id.clone();

    net.add_place(place_a);
    net.add_place(place_b);

    let transition = Transition::new("Pass", "#{ out: inp }")
        .with_input_ports(vec![Port::new("inp")])
        .with_output_ports(vec![Port::new("out")]);
    let transition_id = transition.id.clone();

    net.add_transition(transition);

    net.add_arc(Arc::input(place_a_id, transition_id.clone(), "inp"));
    net.add_arc(Arc::output(transition_id, "out", place_b_id));

    net
}

/// Create a test PetriNet with two transitions.
fn test_net_with_two_transitions() -> PetriNet {
    let mut net = PetriNet::new();

    let place_a = Place::internal("A");
    let place_b = Place::internal("B");
    let place_c = Place::internal("C");
    let place_a_id = place_a.id.clone();
    let place_b_id = place_b.id.clone();
    let place_c_id = place_c.id.clone();

    net.add_place(place_a);
    net.add_place(place_b);
    net.add_place(place_c);

    let t1 = Transition::new("First", "#{ out: inp }")
        .with_input_ports(vec![Port::new("inp")])
        .with_output_ports(vec![Port::new("out")]);
    let t1_id = t1.id.clone();

    let t2 = Transition::new("Second", "#{ out: inp }")
        .with_input_ports(vec![Port::new("inp")])
        .with_output_ports(vec![Port::new("out")]);
    let t2_id = t2.id.clone();

    net.add_transition(t1);
    net.add_transition(t2);

    net.add_arc(Arc::input(place_a_id, t1_id.clone(), "inp"));
    net.add_arc(Arc::output(t1_id, "out", place_b_id.clone()));
    net.add_arc(Arc::input(place_b_id, t2_id.clone(), "inp"));
    net.add_arc(Arc::output(t2_id, "out", place_c_id));

    net
}

/// Assert that set_topology and get_topology work correctly.
pub fn assert_set_and_get(repo: &impl TopologyRepository) {
    // Initially should be empty
    assert!(
        repo.get_topology().is_none(),
        "Repository should be empty initially"
    );

    let net = test_net();
    let expected_places = net.places.len();
    let expected_transitions = net.transitions.len();

    repo.set_topology(net);

    let retrieved = repo
        .get_topology()
        .expect("Should return topology after set");
    assert_eq!(
        retrieved.places.len(),
        expected_places,
        "Place count should match"
    );
    assert_eq!(
        retrieved.transitions.len(),
        expected_transitions,
        "Transition count should match"
    );
}

/// Get the single transition ID from a test net.
fn single_transition_id(net: &PetriNet) -> petri_domain::TransitionId {
    net.transitions
        .keys()
        .next()
        .expect("net must have a transition")
        .clone()
}

/// Get two transition IDs from a test net with two transitions.
fn two_transition_ids(net: &PetriNet) -> (petri_domain::TransitionId, petri_domain::TransitionId) {
    let mut ids: Vec<_> = net.transitions.keys().cloned().collect();
    ids.sort_by_key(|id| net.transitions.get(id).unwrap().name.clone());
    (ids[0].clone(), ids[1].clone())
}

/// Assert that clear removes the topology.
pub fn assert_clear(repo: &impl TopologyRepository) {
    let net = test_net();
    repo.set_topology(net);

    assert!(
        repo.get_topology().is_some(),
        "Should have topology after set"
    );

    repo.clear();

    assert!(repo.get_topology().is_none(), "Should be empty after clear");
}

/// Assert that update_transition_script works for existing transitions.
pub fn assert_update_script_success(repo: &impl TopologyRepository) {
    let net = test_net();
    let transition_id = single_transition_id(&net);
    repo.set_topology(net);

    let new_script = "#{ output: input * 2 }".to_string();
    let new_guard = Some("input > 0".to_string());

    let result =
        repo.update_transition_script(&transition_id, new_script.clone(), new_guard.clone());
    assert!(result, "Should return true for existing transition");

    let updated_net = repo.get_topology().expect("Should have topology");
    let transition = updated_net
        .transitions
        .get(&transition_id)
        .expect("Should find transition");

    assert_eq!(transition.script, new_script, "Script should be updated");
    assert_eq!(transition.guard, new_guard, "Guard should be updated");
}

/// Assert that update_transition_script returns false for non-existent transitions.
pub fn assert_update_script_not_found(repo: &impl TopologyRepository) {
    let net = test_net();
    repo.set_topology(net);

    let fake_id = petri_domain::TransitionId::new();
    let result = repo.update_transition_script(&fake_id, "ignored".to_string(), None);

    assert!(!result, "Should return false for non-existent transition");
}

/// Assert that get_topology returns None when empty.
pub fn assert_get_when_empty(repo: &impl TopologyRepository) {
    assert!(
        repo.get_topology().is_none(),
        "Empty repository should return None"
    );
}

/// Assert that updating one transition preserves others.
pub fn assert_update_preserves_other_transitions(repo: &impl TopologyRepository) {
    let net = test_net_with_two_transitions();
    let (t1_id, t2_id) = two_transition_ids(&net);
    let original_t2_script = net.transitions.get(&t2_id).unwrap().script.clone();

    repo.set_topology(net);

    // Update only t1
    let new_script = "#{ modified: true }".to_string();
    repo.update_transition_script(&t1_id, new_script.clone(), None);

    let updated_net = repo.get_topology().expect("Should have topology");

    // t1 should be updated
    let t1 = updated_net.transitions.get(&t1_id).expect("Should find t1");
    assert_eq!(t1.script, new_script, "t1 script should be updated");

    // t2 should be unchanged
    let t2 = updated_net.transitions.get(&t2_id).expect("Should find t2");
    assert_eq!(
        t2.script, original_t2_script,
        "t2 script should be unchanged"
    );
}

/// Assert that clear is idempotent (multiple clears are safe).
pub fn assert_clear_is_idempotent(repo: &impl TopologyRepository) {
    let net = test_net();
    repo.set_topology(net);

    repo.clear();
    assert!(
        repo.get_topology().is_none(),
        "Should be empty after first clear"
    );

    // Second clear should not panic
    repo.clear();
    assert!(
        repo.get_topology().is_none(),
        "Should be empty after second clear"
    );

    // Third clear on already empty
    repo.clear();
    assert!(repo.get_topology().is_none(), "Should remain empty");
}

/// Run all TopologyRepository assertions against an implementation.
///
/// This is a convenience function that runs all tests in sequence.
/// Prefer using rstest with individual assertions for better test isolation.
pub fn assert_all(repo: &impl TopologyRepository) {
    assert_get_when_empty(repo);

    assert_set_and_get(repo);
    repo.clear();

    assert_clear(repo);
    // already cleared

    let net = test_net();
    repo.set_topology(net);
    assert_update_script_success(repo);
    repo.clear();

    let net = test_net();
    repo.set_topology(net);
    assert_update_script_not_found(repo);
    repo.clear();

    let net = test_net_with_two_transitions();
    repo.set_topology(net);
    assert_update_preserves_other_transitions(repo);
    repo.clear();

    assert_clear_is_idempotent(repo);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doubles::MockTopologyRepository;
    use rstest::rstest;

    #[rstest]
    fn test_set_and_get() {
        assert_set_and_get(&MockTopologyRepository::new());
    }

    #[rstest]
    fn test_clear() {
        assert_clear(&MockTopologyRepository::new());
    }

    #[rstest]
    fn test_update_script_success() {
        assert_update_script_success(&MockTopologyRepository::new());
    }

    #[rstest]
    fn test_update_script_not_found() {
        assert_update_script_not_found(&MockTopologyRepository::new());
    }

    #[rstest]
    fn test_get_when_empty() {
        assert_get_when_empty(&MockTopologyRepository::new());
    }

    #[rstest]
    fn test_update_preserves_other_transitions() {
        assert_update_preserves_other_transitions(&MockTopologyRepository::new());
    }

    #[rstest]
    fn test_clear_is_idempotent() {
        assert_clear_is_idempotent(&MockTopologyRepository::new());
    }
}
