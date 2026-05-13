//! Mock TopologyRepository implementation for testing.

use parking_lot::RwLock;
use petri_application::TopologyRepository;
use petri_domain::{PetriNet, TransitionId};

/// Mock topology repository with change tracking.
///
/// Features:
/// - Tracks all topology updates
/// - Records script update calls for verification
/// - Thread-safe with RwLock
pub struct MockTopologyRepository {
    topology: RwLock<Option<PetriNet>>,
    set_count: RwLock<usize>,
    script_updates: RwLock<Vec<(TransitionId, String, Option<String>)>>,
}

impl MockTopologyRepository {
    /// Create a new empty mock repository.
    pub fn new() -> Self {
        Self {
            topology: RwLock::new(None),
            set_count: RwLock::new(0),
            script_updates: RwLock::new(Vec::new()),
        }
    }

    /// Create with a pre-loaded topology.
    pub fn with_topology(net: PetriNet) -> Self {
        Self {
            topology: RwLock::new(Some(net)),
            set_count: RwLock::new(0),
            script_updates: RwLock::new(Vec::new()),
        }
    }

    /// Get the number of times set_topology was called.
    pub fn set_count(&self) -> usize {
        *self.set_count.read()
    }

    /// Get all script updates for verification: (transition_id, script, guard).
    pub fn script_updates(&self) -> Vec<(TransitionId, String, Option<String>)> {
        self.script_updates.read().clone()
    }

    /// Check if topology has been set.
    pub fn has_topology(&self) -> bool {
        self.topology.read().is_some()
    }
}

impl Default for MockTopologyRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl TopologyRepository for MockTopologyRepository {
    fn get_topology(&self) -> Option<PetriNet> {
        self.topology.read().clone()
    }

    fn set_topology(&self, net: PetriNet) {
        *self.set_count.write() += 1;
        *self.topology.write() = Some(net);
    }

    fn clear(&self) {
        *self.topology.write() = None;
    }

    fn update_transition_script(
        &self,
        transition_id: &TransitionId,
        script: String,
        guard: Option<String>,
    ) -> bool {
        self.script_updates
            .write()
            .push((transition_id.clone(), script.clone(), guard.clone()));

        if let Some(ref mut net) = *self.topology.write() {
            if let Some(t) = net.transitions.get_mut(transition_id) {
                t.script = script;
                t.guard = guard;
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_is_empty() {
        let repo = MockTopologyRepository::new();
        assert!(!repo.has_topology());
        assert_eq!(repo.set_count(), 0);
    }

    #[test]
    fn test_set_topology() {
        let repo = MockTopologyRepository::new();
        let net = PetriNet::new();

        repo.set_topology(net);

        assert!(repo.has_topology());
        assert_eq!(repo.set_count(), 1);
    }

    #[test]
    fn test_clear() {
        let net = PetriNet::new();
        let repo = MockTopologyRepository::with_topology(net);

        assert!(repo.has_topology());
        repo.clear();
        assert!(!repo.has_topology());
    }
}
