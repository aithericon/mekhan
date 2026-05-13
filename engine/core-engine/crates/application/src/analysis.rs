//! Static analysis for Petri Net topology validation.
//!
//! This module provides semantic validation of Petri Net structures based on:
//! - Place type semantics (State, Signal)
//! - Port wiring invariants (input/output connections)
//! - Cardinality constraints

use std::collections::HashMap;

use petri_domain::{ArcDirection, PetriNet, PlaceKind, PortCardinality};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// =============================================================================
// Analysis Types (defined here to avoid circular dependency with petri_api)
// =============================================================================

/// Severity level of a validation issue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IssueLevel {
    /// Critical issue - will cause runtime errors or halt execution
    Error,
    /// Logical flaw or potential problem
    Warning,
    /// Suggestion or informational message
    Info,
}

/// A single validation issue found during static analysis.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ValidationIssue {
    /// ID of the node (place or transition) with the issue
    pub node_id: String,
    /// Type of node: "place" or "transition"
    pub node_type: String,
    /// Severity level
    pub level: IssueLevel,
    /// Issue code (e.g., "UNREACHABLE", "DEAD_END", "DISCONNECTED_INPUT")
    pub code: String,
    /// Human-readable description of the issue
    pub message: String,
    /// For cross-net bridge issues: the remote net involved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_net_id: Option<String>,
}

/// Summary of validation results by severity.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct AnalysisSummary {
    pub error_count: usize,
    pub warning_count: usize,
    pub info_count: usize,
}

/// Complete analysis report for a Petri Net topology.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct AnalysisReport {
    /// True if there are no errors (warnings and info are allowed)
    pub is_valid: bool,
    /// List of all validation issues found
    pub issues: Vec<ValidationIssue>,
    /// Summary counts by severity
    pub summary: AnalysisSummary,
}

/// Helper to create an error-level issue
fn error(
    node_id: &str,
    node_type: &str,
    code: &str,
    message: impl Into<String>,
) -> ValidationIssue {
    ValidationIssue {
        node_id: node_id.to_string(),
        node_type: node_type.to_string(),
        level: IssueLevel::Error,
        code: code.to_string(),
        message: message.into(),
        remote_net_id: None,
    }
}

/// Helper to create a warning-level issue
fn warning(
    node_id: &str,
    node_type: &str,
    code: &str,
    message: impl Into<String>,
) -> ValidationIssue {
    ValidationIssue {
        node_id: node_id.to_string(),
        node_type: node_type.to_string(),
        level: IssueLevel::Warning,
        code: code.to_string(),
        message: message.into(),
        remote_net_id: None,
    }
}

/// Validate a Petri Net topology and return an analysis report.
///
/// Checks performed:
/// 1. **Place Heuristics**: Based on PlaceKind semantics
///    - Internal: Should have inputs and outputs
///    - Signal/BridgeIn: Externally fed, should trigger something (outputs)
///    - BridgeOut: Should have inputs, tokens leave via bridge
///    - BridgeReply: Receives via reply_routing routing, tokens leave via bridge
///
/// 2. **Port Invariants**: Strict wiring checks
///    - All input ports must have incoming arcs (Error if missing)
///    - All output ports should have outgoing arcs (Warning if missing)
///    - Single cardinality ports must have arc weight == 1
pub fn validate_topology(net: &PetriNet) -> AnalysisReport {
    let mut issues = Vec::new();

    // 1. Calculate place degrees (in/out connectivity)
    let mut place_in_degree: HashMap<String, usize> = HashMap::new();
    let mut place_out_degree: HashMap<String, usize> = HashMap::new();

    for arc in &net.arcs {
        let place_id = arc.place_id.to_string();
        match arc.direction {
            ArcDirection::PlaceToTransition => {
                *place_out_degree.entry(place_id).or_default() += 1;
            }
            ArcDirection::TransitionToPlace => {
                *place_in_degree.entry(place_id).or_default() += 1;
            }
        }
    }

    // 2. Validate places by type
    for place in net.places.values() {
        let place_id_str = place.id.to_string();
        let in_d = *place_in_degree.get(&place_id_str).unwrap_or(&0);
        let out_d = *place_out_degree.get(&place_id_str).unwrap_or(&0);

        match &place.kind {
            PlaceKind::Internal => {
                if in_d == 0 {
                    issues.push(warning(
                        &place_id_str,
                        "place",
                        "UNREACHABLE",
                        format!(
                            "'{}': No inputs - may be unreachable without initial tokens",
                            place.name
                        ),
                    ));
                }
                // Places with no outputs are natural terminal/sink states — no warning needed.
            }
            PlaceKind::Signal | PlaceKind::BridgeIn { .. } => {
                // Externally fed — skip UNREACHABLE
                if out_d == 0 && !place.is_bridge_out() {
                    issues.push(warning(
                        &place_id_str,
                        "place",
                        "UNUSED_SIGNAL",
                        format!("'{}': Signal not connected to any transition", place.name),
                    ));
                }
            }
            PlaceKind::BridgeOut { .. } => {
                if in_d == 0 {
                    issues.push(warning(
                        &place_id_str,
                        "place",
                        "UNREACHABLE",
                        format!(
                            "'{}': No inputs - may be unreachable without initial tokens",
                            place.name
                        ),
                    ));
                }
                // No DEAD_END — tokens leave via bridge
            }
            PlaceKind::BridgeReply { .. } => {
                // Receives via reply_routing routing — skip UNREACHABLE
                // No DEAD_END — tokens leave via bridge
            }
            PlaceKind::Terminal => {
                // Terminal sink — tokens here signal completion.
                // No DEAD_END warning (by design). Still check reachability.
                if in_d == 0 {
                    issues.push(warning(
                        &place_id_str,
                        "place",
                        "UNREACHABLE",
                        format!(
                            "'{}': Terminal place has no incoming arcs — can never complete",
                            place.name
                        ),
                    ));
                }
            }
        }
    }

    // 3. Validate transition ports
    for transition in net.transitions.values() {
        let transition_id_str = transition.id.to_string();
        let input_arcs = net.input_arcs(&transition.id);
        let output_arcs = net.output_arcs(&transition.id);

        // Check all input ports are wired
        for port in &transition.input_ports {
            let wired = input_arcs.iter().any(|a| a.port_name == port.name);
            if !wired {
                issues.push(error(
                    &transition_id_str,
                    "transition",
                    "DISCONNECTED_INPUT",
                    format!(
                        "'{}': Input port '{}' has no incoming arc",
                        transition.name, port.name
                    ),
                ));
            }

            // Check cardinality match for Single ports
            if port.cardinality == PortCardinality::Single {
                for arc in input_arcs.iter().filter(|a| a.port_name == port.name) {
                    if arc.weight > 1 {
                        issues.push(error(
                            &transition_id_str,
                            "transition",
                            "CARDINALITY_MISMATCH",
                            format!(
                                "'{}': Port '{}' expects Single but arc weight is {}",
                                transition.name, port.name, arc.weight
                            ),
                        ));
                    }
                }
            }
        }

        // Check output ports are wired
        for port in &transition.output_ports {
            let wired = output_arcs.iter().any(|a| a.port_name == port.name);
            if !wired {
                // Skip _error ports on effect transitions — error routing is optional by design
                if transition.is_effect() && port.name == "_error" {
                    continue;
                }
                issues.push(warning(
                    &transition_id_str,
                    "transition",
                    "DISCONNECTED_OUTPUT",
                    format!(
                        "'{}': Output port '{}' has no outgoing arc - data discarded",
                        transition.name, port.name
                    ),
                ));
            }
        }
    }

    // Build summary
    let error_count = issues
        .iter()
        .filter(|i| i.level == IssueLevel::Error)
        .count();
    let warning_count = issues
        .iter()
        .filter(|i| i.level == IssueLevel::Warning)
        .count();
    let info_count = issues
        .iter()
        .filter(|i| i.level == IssueLevel::Info)
        .count();

    AnalysisReport {
        is_valid: error_count == 0,
        issues,
        summary: AnalysisSummary {
            error_count,
            warning_count,
            info_count,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::{Arc, Place, Port, Transition};

    #[test]
    fn test_empty_net_is_valid() {
        let net = PetriNet::new();
        let report = validate_topology(&net);
        assert!(report.is_valid);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn test_state_with_no_inputs_is_unreachable() {
        let mut net = PetriNet::new();
        let place = Place::internal("Orphan State");
        net.add_place(place);

        let report = validate_topology(&net);
        assert!(report.issues.iter().any(|i| i.code == "UNREACHABLE"));
    }

    #[test]
    fn test_state_with_no_outputs_is_valid_sink() {
        let mut net = PetriNet::new();

        // Create a simple net: place1 -> transition -> place2 (sink / terminal)
        let p1 = Place::internal("Start");
        let p2 = Place::internal("End");
        let p1_id = net.add_place(p1);
        let p2_id = net.add_place(p2);

        let t = Transition::new("move", "#{out: inp}")
            .with_input_port(Port::new("inp"))
            .with_output_port(Port::new("out"));
        let t_id = net.add_transition(t);

        net.add_arc(Arc::input(p1_id, t_id.clone(), "inp"));
        net.add_arc(Arc::output(t_id, "out", p2_id));

        let report = validate_topology(&net);
        // Sink places are natural terminal states — no DEAD_END warning
        assert!(!report.issues.iter().any(|i| i.code == "DEAD_END"));
    }

    #[test]
    fn test_disconnected_input_port_is_error() {
        let mut net = PetriNet::new();

        let place = Place::internal("Input");
        let place_id = net.add_place(place);

        // Transition has two input ports, but only one is wired
        let t = Transition::new("process", "#{}")
            .with_input_port(Port::new("wired"))
            .with_input_port(Port::new("unwired"));
        let t_id = net.add_transition(t);

        net.add_arc(Arc::input(place_id, t_id, "wired"));

        let report = validate_topology(&net);
        assert!(!report.is_valid);
        assert!(report
            .issues
            .iter()
            .any(|i| i.code == "DISCONNECTED_INPUT" && i.message.contains("unwired")));
    }

    #[test]
    fn test_signal_with_no_outputs_is_unused() {
        let mut net = PetriNet::new();
        let signal = Place::signal("Unused Signal");
        net.add_place(signal);

        let report = validate_topology(&net);
        assert!(report.issues.iter().any(|i| i.code == "UNUSED_SIGNAL"));
    }

    #[test]
    fn test_disconnected_output_port_is_warning() {
        let mut net = PetriNet::new();

        let input_place = Place::internal("Input");
        let output_place = Place::internal("Output");
        let i_id = net.add_place(input_place);
        let o_id = net.add_place(output_place);

        // Transition has two output ports, but only one is wired
        let t = Transition::new("leak", "#{}")
            .with_input_port(Port::new("inp"))
            .with_output_port(Port::new("wired"))
            .with_output_port(Port::new("unwired"));
        let t_id = net.add_transition(t);

        net.add_arc(Arc::input(i_id, t_id.clone(), "inp"));
        net.add_arc(Arc::output(t_id, "wired", o_id));

        let report = validate_topology(&net);
        // Should still be valid (disconnected output is just a warning)
        assert!(report.is_valid);
        assert!(report
            .issues
            .iter()
            .any(|i| i.code == "DISCONNECTED_OUTPUT" && i.message.contains("unwired")));
    }

    #[test]
    fn test_cardinality_mismatch_is_error() {
        let mut net = PetriNet::new();

        let place = Place::internal("Multi-source");
        let dest = Place::internal("Destination");
        let p_id = net.add_place(place);
        let d_id = net.add_place(dest);

        // Single cardinality port with weight > 1 arc
        let t = Transition::new("process", "#{out: inp}")
            .with_input_port(Port::new("inp")) // Default is Single
            .with_output_port(Port::new("out"));
        let t_id = net.add_transition(t);

        // Create an arc with weight > 1
        let mut arc = Arc::input(p_id, t_id.clone(), "inp");
        arc.weight = 3; // Mismatch!
        net.add_arc(arc);
        net.add_arc(Arc::output(t_id, "out", d_id));

        let report = validate_topology(&net);
        assert!(!report.is_valid);
        assert!(report
            .issues
            .iter()
            .any(|i| i.code == "CARDINALITY_MISMATCH"));
    }

    #[test]
    fn test_well_formed_circular_net_is_valid() {
        let mut net = PetriNet::new();

        // Create a simple circular net: idle -> work -> idle
        let idle = Place::internal("Idle Workers");
        let working = Place::internal("Working");
        let i_id = net.add_place(idle);
        let w_id = net.add_place(working);

        let start_work = Transition::new("start", "#{out: inp}")
            .with_input_port(Port::new("inp"))
            .with_output_port(Port::new("out"));
        let finish_work = Transition::new("finish", "#{out: inp}")
            .with_input_port(Port::new("inp"))
            .with_output_port(Port::new("out"));

        let t1_id = net.add_transition(start_work);
        let t2_id = net.add_transition(finish_work);

        net.add_arc(Arc::input(i_id.clone(), t1_id.clone(), "inp"));
        net.add_arc(Arc::output(t1_id, "out", w_id.clone()));
        net.add_arc(Arc::input(w_id, t2_id.clone(), "inp"));
        net.add_arc(Arc::output(t2_id, "out", i_id));

        let report = validate_topology(&net);
        assert!(report.is_valid);
        assert_eq!(report.summary.error_count, 0);
    }

    #[test]
    fn test_summary_counts_are_correct() {
        let mut net = PetriNet::new();

        // Add nodes that will generate specific issues:
        // 1 error: disconnected input port
        // 2 warnings: orphan state (unreachable + dead_end)

        // Error: Transition with disconnected input
        let t = Transition::new("broken", "#{}").with_input_port(Port::new("orphan_port"));
        net.add_transition(t);

        // Warning: Orphan state with no connections (UNREACHABLE only, no DEAD_END)
        let orphan = Place::internal("Orphan");
        net.add_place(orphan);

        let report = validate_topology(&net);

        // Verify we have at least the expected issues
        assert!(report.summary.error_count >= 1, "Expected at least 1 error");
        assert!(
            report.summary.warning_count >= 1,
            "Expected at least 1 warning"
        );
        assert!(!report.is_valid, "Net should be invalid due to error");
    }

    #[test]
    fn test_signal_with_outputs_has_no_issues() {
        let mut net = PetriNet::new();

        let signal = Place::signal("Active Signal");
        let dest = Place::internal("Destination");
        let s_id = net.add_place(signal);
        let d_id = net.add_place(dest);

        let t = Transition::new("trigger", "#{out: inp}")
            .with_input_port(Port::new("inp"))
            .with_output_port(Port::new("out"));
        let t_id = net.add_transition(t);

        net.add_arc(Arc::input(s_id, t_id.clone(), "inp"));
        net.add_arc(Arc::output(t_id, "out", d_id));

        let report = validate_topology(&net);
        // Signal should not have UNUSED_SIGNAL issue
        assert!(!report.issues.iter().any(|i| i.code == "UNUSED_SIGNAL"));
    }

    // =========================================================================
    // Bridge / Effect suppression tests
    // =========================================================================

    #[test]
    fn test_bridge_out_state_no_dead_end() {
        let mut net = PetriNet::new();
        let place = Place::bridge_out("Outbound", "remote-net", "inbox");
        net.add_place(place);

        let report = validate_topology(&net);
        assert!(!report.issues.iter().any(|i| i.code == "DEAD_END"));
    }

    #[test]
    fn test_bridge_reply_state_no_unreachable() {
        let mut net = PetriNet::new();
        let place = Place::bridge_reply("Reply Inbox");
        net.add_place(place);

        let report = validate_topology(&net);
        assert!(!report.issues.iter().any(|i| i.code == "UNREACHABLE"));
    }

    #[test]
    fn test_bridge_out_signal_no_unused() {
        let mut net = PetriNet::new();
        let place = Place::bridge_out("Remote Signal", "remote-net", "signal_in");
        net.add_place(place);

        let report = validate_topology(&net);
        assert!(!report.issues.iter().any(|i| i.code == "UNUSED_SIGNAL"));
    }

    #[test]
    fn test_effect_error_port_not_warned() {
        let mut net = PetriNet::new();

        let input_place = Place::internal("Input");
        let output_place = Place::internal("Output");
        let i_id = net.add_place(input_place);
        let o_id = net.add_place(output_place);

        let t = Transition::new("call_api", "#{out: inp}")
            .with_input_port(Port::new("inp"))
            .with_output_port(Port::new("out"))
            .with_output_port(Port::new("_error"))
            .with_effect_handler("http_handler");
        let t_id = net.add_transition(t);

        net.add_arc(Arc::input(i_id, t_id.clone(), "inp"));
        net.add_arc(Arc::output(t_id, "out", o_id));
        // _error port intentionally left unwired

        let report = validate_topology(&net);
        assert!(!report
            .issues
            .iter()
            .any(|i| i.code == "DISCONNECTED_OUTPUT" && i.message.contains("_error")));
    }
}
