use std::sync::RwLock;

use petri_application::TopologyRepository;
use petri_domain::{PetriNet, TransitionId};

/// In-memory implementation of the topology store.
pub struct MemoryTopologyStore {
    topology: RwLock<Option<PetriNet>>,
}

impl MemoryTopologyStore {
    pub fn new() -> Self {
        Self {
            topology: RwLock::new(None),
        }
    }
}

impl Default for MemoryTopologyStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TopologyRepository for MemoryTopologyStore {
    fn get_topology(&self) -> Option<PetriNet> {
        self.topology.read().unwrap().clone()
    }

    fn set_topology(&self, net: PetriNet) {
        *self.topology.write().unwrap() = Some(net);
    }

    fn clear(&self) {
        *self.topology.write().unwrap() = None;
    }

    fn update_transition_script(
        &self,
        transition_id: &TransitionId,
        script: String,
        guard: Option<String>,
    ) -> bool {
        let mut topology = self.topology.write().unwrap();
        if let Some(ref mut net) = *topology {
            if let Some(transition) = net.transitions.get_mut(transition_id) {
                transition.script = script;
                transition.guard = guard;
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_test_harness::prelude::*;

    #[rstest]
    fn test_set_and_get() {
        assert_set_and_get(&MemoryTopologyStore::new());
    }

    #[rstest]
    fn test_clear() {
        assert_clear(&MemoryTopologyStore::new());
    }

    #[rstest]
    fn test_update_script_success() {
        assert_update_script_success(&MemoryTopologyStore::new());
    }

    #[rstest]
    fn test_update_script_not_found() {
        assert_update_script_not_found(&MemoryTopologyStore::new());
    }

    #[rstest]
    fn test_get_when_empty() {
        assert_get_when_empty(&MemoryTopologyStore::new());
    }

    #[rstest]
    fn test_update_preserves_other_transitions() {
        assert_update_preserves_other_transitions(&MemoryTopologyStore::new());
    }

    #[rstest]
    fn test_clear_is_idempotent() {
        assert_clear_is_idempotent(&MemoryTopologyStore::new());
    }
}
