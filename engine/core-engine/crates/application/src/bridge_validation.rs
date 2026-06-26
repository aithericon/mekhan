//! Cross-net bridge validation.
//!
//! Validates that bridge_out/bridge_in connections between nets are consistent:
//! target nets exist, target places exist and are the right kind, reply places
//! exist locally, and source annotations match.

use petri_domain::{PetriNet, PlaceKind};

use crate::analysis::{AnalysisReport, AnalysisSummary, IssueLevel, ValidationIssue};

// ---------------------------------------------------------------------------
// Resolver trait (implemented in petri-api against NetRegistry)
// ---------------------------------------------------------------------------

/// Resolves net topologies by ID for cross-net validation.
///
/// Defined here (application layer) so bridge_validation stays independent
/// of the API crate. The API crate provides the concrete implementation.
pub trait NetTopologyResolver {
    /// Return the topology for a deployed net, or None if not loaded.
    fn resolve_topology(&self, net_id: &str) -> Option<PetriNet>;

    /// Return all currently deployed net IDs (for fuzzy matching).
    fn all_net_ids(&self) -> Vec<String>;
}

// ---------------------------------------------------------------------------
// Validation mode
// ---------------------------------------------------------------------------

/// Controls whether unresolved references are warnings or errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeValidationMode {
    /// Deploy-time: target nets may not exist yet — missing targets are warnings.
    Warn,
    /// Run-mode transition: all nets should be present — missing targets are errors.
    Strict,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Validate all bridge connections for a single net against other deployed nets.
pub fn validate_bridges(
    net_id: &str,
    net: &PetriNet,
    resolver: &dyn NetTopologyResolver,
    mode: BridgeValidationMode,
) -> AnalysisReport {
    let mut issues = Vec::new();
    let all_net_ids = resolver.all_net_ids();

    for place in net.places.values() {
        let place_id = place.id.to_string();

        match &place.kind {
            PlaceKind::BridgeOut {
                target_net_id,
                target_place_name,
                reply_to,
                reply_channels,
                ..
            } => {
                // Skip dynamic targets ($params.xxx, $result.xxx)
                if target_net_id.contains('$') || target_place_name.contains('$') {
                    issues.push(info_issue(
                        &place_id,
                        "place",
                        "BRIDGE_DYNAMIC_TARGET",
                        format!(
                            "'{}': Dynamic bridge target ({} / {}) — skipping static validation",
                            place.name, target_net_id, target_place_name
                        ),
                        Some(target_net_id.clone()),
                    ));
                    continue;
                }

                // Check target net exists
                let target_net = match resolver.resolve_topology(target_net_id) {
                    Some(net) => net,
                    None => {
                        let mut msg = format!(
                            "'{}': Target net '{}' is not deployed",
                            place.name, target_net_id
                        );
                        if let Some(suggestion) = suggest_closest(target_net_id, &all_net_ids) {
                            msg.push_str(&format!(". Did you mean '{}'?", suggestion));
                        }
                        issues.push(mode_issue(
                            mode,
                            &place_id,
                            "place",
                            "BRIDGE_TARGET_NET_MISSING",
                            msg,
                            Some(target_net_id.clone()),
                        ));
                        continue;
                    }
                };

                // Check target place exists
                let target_place = target_net
                    .places
                    .values()
                    .find(|p| p.name == *target_place_name || p.id.0 == *target_place_name);

                match target_place {
                    None => {
                        let candidate_names: Vec<&str> = target_net
                            .places
                            .values()
                            .filter(|p| matches!(p.kind, PlaceKind::BridgeIn { .. }))
                            .map(|p| p.name.as_str())
                            .collect();

                        let mut msg = format!(
                            "'{}': Place '{}' not found in net '{}'",
                            place.name, target_place_name, target_net_id
                        );
                        if let Some(suggestion) =
                            suggest_closest(target_place_name, &candidate_names)
                        {
                            msg.push_str(&format!(". Did you mean '{}'?", suggestion));
                        }
                        issues.push(error_issue(
                            &place_id,
                            "place",
                            "BRIDGE_TARGET_PLACE_MISSING",
                            msg,
                            Some(target_net_id.clone()),
                        ));
                    }
                    Some(tp) => {
                        // Check the target place is actually a BridgeIn
                        if !matches!(tp.kind, PlaceKind::BridgeIn { .. }) {
                            issues.push(error_issue(
                                &place_id,
                                "place",
                                "BRIDGE_TARGET_NOT_BRIDGE_IN",
                                format!(
                                    "'{}': Target place '{}' in net '{}' is {:?}, not bridge_in",
                                    place.name,
                                    target_place_name,
                                    target_net_id,
                                    kind_label(&tp.kind)
                                ),
                                Some(target_net_id.clone()),
                            ));
                        }
                    }
                }

                // Check reply_to references a local place
                if let Some(reply_place_name) = reply_to {
                    if !local_place_exists(net, reply_place_name) {
                        issues.push(error_issue(
                            &place_id,
                            "place",
                            "BRIDGE_REPLY_PLACE_MISSING",
                            format!(
                                "'{}': reply_to '{}' does not exist in this net",
                                place.name, reply_place_name
                            ),
                            None,
                        ));
                    }
                }

                // Check reply_channels values reference local places
                if let Some(channels) = reply_channels {
                    for (channel_name, local_place_name) in channels {
                        if !local_place_exists(net, local_place_name) {
                            issues.push(error_issue(
                                &place_id,
                                "place",
                                "BRIDGE_REPLY_PLACE_MISSING",
                                format!(
                                    "'{}': reply_channel '{}' → '{}' does not exist in this net",
                                    place.name, channel_name, local_place_name
                                ),
                                None,
                            ));
                        }
                    }
                }
            }

            PlaceKind::BridgeIn {
                source_net_id: Some(src_net_id),
                source_place_name: Some(src_place_name),
            } => {
                // Skip dynamic references
                if src_net_id.contains('$') || src_place_name.contains('$') {
                    continue;
                }

                // Check source net exists
                let source_net = match resolver.resolve_topology(src_net_id) {
                    Some(net) => net,
                    None => {
                        issues.push(mode_issue(
                            mode,
                            &place_id,
                            "place",
                            "BRIDGE_SOURCE_NET_MISSING",
                            format!(
                                "'{}': Annotated source net '{}' is not deployed",
                                place.name, src_net_id
                            ),
                            Some(src_net_id.clone()),
                        ));
                        continue;
                    }
                };

                // Check that the source net has a bridge_out targeting this net+place
                let has_matching_bridge_out = source_net.places.values().any(|p| {
                    if let PlaceKind::BridgeOut {
                        target_net_id,
                        target_place_name,
                        ..
                    } = &p.kind
                    {
                        target_net_id == net_id
                            && (target_place_name == &place.name
                                || target_place_name == &place.id.0)
                    } else {
                        false
                    }
                });

                if !has_matching_bridge_out {
                    issues.push(warning_issue(
                        &place_id,
                        "place",
                        "BRIDGE_SOURCE_MISMATCH",
                        format!(
                            "'{}': Expects tokens from '{}' place '{}', \
                             but no bridge_out in '{}' targets '{}:{}'",
                            place.name, src_net_id, src_place_name, src_net_id, net_id, place.name
                        ),
                        Some(src_net_id.clone()),
                    ));
                }
            }

            _ => {} // Other place kinds don't need cross-net checks
        }
    }

    build_report(issues)
}

/// Validate all bridge connections across every deployed net.
pub fn validate_all_bridges(resolver: &dyn NetTopologyResolver) -> AnalysisReport {
    let net_ids = resolver.all_net_ids();
    let mut all_issues = Vec::new();

    for net_id in &net_ids {
        if let Some(net) = resolver.resolve_topology(net_id) {
            let report = validate_bridges(net_id, &net, resolver, BridgeValidationMode::Strict);
            for mut issue in report.issues {
                // Prefix node_id with net_id for disambiguation
                issue.node_id = format!("{}/{}", net_id, issue.node_id);
                all_issues.push(issue);
            }
        }
    }

    build_report(all_issues)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn local_place_exists(net: &PetriNet, name: &str) -> bool {
    net.places
        .values()
        .any(|p| p.name == name || p.id.0 == name)
}

fn kind_label(kind: &PlaceKind) -> &'static str {
    match kind {
        PlaceKind::Internal => "internal",
        PlaceKind::Signal => "signal",
        PlaceKind::BridgeIn { .. } => "bridge_in",
        PlaceKind::BridgeOut { .. } => "bridge_out",
        PlaceKind::BridgeReply { .. } => "bridge_reply",
        PlaceKind::Terminal => "terminal",
        PlaceKind::Sink => "sink",
    }
}

/// Find the closest match above a similarity threshold.
fn suggest_closest<S: AsRef<str>>(target: &str, candidates: &[S]) -> Option<String> {
    let mut best: Option<(String, f64)> = None;
    for candidate in candidates {
        let score = strsim::jaro_winkler(target, candidate.as_ref());
        if score > 0.75 && (best.is_none() || score > best.as_ref().unwrap().1) {
            best = Some((candidate.as_ref().to_string(), score));
        }
    }
    best.map(|(name, _)| name)
}

fn build_report(issues: Vec<ValidationIssue>) -> AnalysisReport {
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

/// Create an issue whose level depends on the validation mode.
/// In Warn mode: warning. In Strict mode: error.
fn mode_issue(
    mode: BridgeValidationMode,
    node_id: &str,
    node_type: &str,
    code: &str,
    message: impl Into<String>,
    remote_net_id: Option<String>,
) -> ValidationIssue {
    ValidationIssue {
        node_id: node_id.to_string(),
        node_type: node_type.to_string(),
        level: match mode {
            BridgeValidationMode::Warn => IssueLevel::Warning,
            BridgeValidationMode::Strict => IssueLevel::Error,
        },
        code: code.to_string(),
        message: message.into(),
        remote_net_id,
    }
}

fn error_issue(
    node_id: &str,
    node_type: &str,
    code: &str,
    message: impl Into<String>,
    remote_net_id: Option<String>,
) -> ValidationIssue {
    ValidationIssue {
        node_id: node_id.to_string(),
        node_type: node_type.to_string(),
        level: IssueLevel::Error,
        code: code.to_string(),
        message: message.into(),
        remote_net_id,
    }
}

fn warning_issue(
    node_id: &str,
    node_type: &str,
    code: &str,
    message: impl Into<String>,
    remote_net_id: Option<String>,
) -> ValidationIssue {
    ValidationIssue {
        node_id: node_id.to_string(),
        node_type: node_type.to_string(),
        level: IssueLevel::Warning,
        code: code.to_string(),
        message: message.into(),
        remote_net_id,
    }
}

fn info_issue(
    node_id: &str,
    node_type: &str,
    code: &str,
    message: impl Into<String>,
    remote_net_id: Option<String>,
) -> ValidationIssue {
    ValidationIssue {
        node_id: node_id.to_string(),
        node_type: node_type.to_string(),
        level: IssueLevel::Info,
        code: code.to_string(),
        message: message.into(),
        remote_net_id,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::Place;
    use std::collections::HashMap;

    /// In-memory resolver for tests.
    struct MockResolver {
        nets: HashMap<String, PetriNet>,
    }

    impl MockResolver {
        fn new() -> Self {
            Self {
                nets: HashMap::new(),
            }
        }

        fn add_net(&mut self, id: &str, net: PetriNet) {
            self.nets.insert(id.to_string(), net);
        }
    }

    impl NetTopologyResolver for MockResolver {
        fn resolve_topology(&self, net_id: &str) -> Option<PetriNet> {
            self.nets.get(net_id).cloned()
        }

        fn all_net_ids(&self) -> Vec<String> {
            self.nets.keys().cloned().collect()
        }
    }

    #[test]
    fn happy_path_matching_bridge_pair() {
        let mut net_a = PetriNet::new();
        net_a.add_place(Place::bridge_out("outbox", "net-b", "inbox"));

        let mut net_b = PetriNet::new();
        net_b.add_place(Place::bridge_in_from("inbox", "net-a", "outbox"));

        let mut resolver = MockResolver::new();
        resolver.add_net("net-a", net_a.clone());
        resolver.add_net("net-b", net_b);

        let report = validate_bridges("net-a", &net_a, &resolver, BridgeValidationMode::Strict);
        assert!(report.is_valid);
        assert_eq!(report.summary.error_count, 0);
    }

    #[test]
    fn missing_target_net_warn_mode() {
        let mut net_a = PetriNet::new();
        net_a.add_place(Place::bridge_out("outbox", "net-b", "inbox"));

        let mut resolver = MockResolver::new();
        resolver.add_net("net-a", net_a.clone());
        // net-b not deployed

        let report = validate_bridges("net-a", &net_a, &resolver, BridgeValidationMode::Warn);
        // In Warn mode, missing net is a warning, not error
        assert!(report.is_valid);
        assert_eq!(report.summary.warning_count, 1);
        assert!(report.issues[0].code == "BRIDGE_TARGET_NET_MISSING");
    }

    #[test]
    fn missing_target_net_strict_mode() {
        let mut net_a = PetriNet::new();
        net_a.add_place(Place::bridge_out("outbox", "net-b", "inbox"));

        let mut resolver = MockResolver::new();
        resolver.add_net("net-a", net_a.clone());

        let report = validate_bridges("net-a", &net_a, &resolver, BridgeValidationMode::Strict);
        assert!(!report.is_valid);
        assert_eq!(report.summary.error_count, 1);
        assert!(report.issues[0].code == "BRIDGE_TARGET_NET_MISSING");
    }

    #[test]
    fn missing_target_place_with_suggestion() {
        let mut net_a = PetriNet::new();
        net_a.add_place(Place::bridge_out("outbox", "net-b", "optimize_inbx"));

        let mut net_b = PetriNet::new();
        net_b.add_place(Place::bridge_in("optimize_inbox"));

        let mut resolver = MockResolver::new();
        resolver.add_net("net-a", net_a.clone());
        resolver.add_net("net-b", net_b);

        let report = validate_bridges("net-a", &net_a, &resolver, BridgeValidationMode::Strict);
        assert!(!report.is_valid);
        let issue = &report.issues[0];
        assert_eq!(issue.code, "BRIDGE_TARGET_PLACE_MISSING");
        assert!(issue.message.contains("Did you mean"));
    }

    #[test]
    fn target_place_wrong_kind() {
        let mut net_a = PetriNet::new();
        net_a.add_place(Place::bridge_out("outbox", "net-b", "inbox"));

        let mut net_b = PetriNet::new();
        net_b.add_place(Place::internal("inbox")); // Should be bridge_in

        let mut resolver = MockResolver::new();
        resolver.add_net("net-a", net_a.clone());
        resolver.add_net("net-b", net_b);

        let report = validate_bridges("net-a", &net_a, &resolver, BridgeValidationMode::Strict);
        assert!(!report.is_valid);
        assert_eq!(report.issues[0].code, "BRIDGE_TARGET_NOT_BRIDGE_IN");
    }

    #[test]
    fn dynamic_target_skipped() {
        let mut net_a = PetriNet::new();
        net_a.add_place(Place::bridge_out_labeled(
            "outbox",
            "$result.child_net_id",
            "inbox",
            None,
            "Spawned Child",
        ));

        let resolver = MockResolver::new();
        let report = validate_bridges("net-a", &net_a, &resolver, BridgeValidationMode::Strict);
        assert!(report.is_valid);
        assert_eq!(report.summary.info_count, 1);
        assert_eq!(report.issues[0].code, "BRIDGE_DYNAMIC_TARGET");
    }

    #[test]
    fn reply_place_missing() {
        let mut net_a = PetriNet::new();
        net_a.add_place(Place::bridge_out_reply(
            "outbox",
            "net-b",
            "inbox",
            "nonexistent_reply",
        ));

        let mut net_b = PetriNet::new();
        net_b.add_place(Place::bridge_in("inbox"));

        let mut resolver = MockResolver::new();
        resolver.add_net("net-a", net_a.clone());
        resolver.add_net("net-b", net_b);

        let report = validate_bridges("net-a", &net_a, &resolver, BridgeValidationMode::Strict);
        assert!(!report.is_valid);
        assert!(report
            .issues
            .iter()
            .any(|i| i.code == "BRIDGE_REPLY_PLACE_MISSING"));
    }

    #[test]
    fn source_mismatch_on_bridge_in() {
        let net_a = PetriNet::new();
        // net_a does NOT have a bridge_out targeting net-b

        let mut net_b = PetriNet::new();
        net_b.add_place(Place::bridge_in_from("inbox", "net-a", "outbox"));

        let mut resolver = MockResolver::new();
        resolver.add_net("net-a", net_a);
        resolver.add_net("net-b", net_b.clone());

        let report = validate_bridges("net-b", &net_b, &resolver, BridgeValidationMode::Strict);
        // Source mismatch is always a warning (not error)
        assert!(report.is_valid);
        assert_eq!(report.summary.warning_count, 1);
        assert_eq!(report.issues[0].code, "BRIDGE_SOURCE_MISMATCH");
    }

    #[test]
    fn validate_all_bridges_prefixes_net_id() {
        let mut net_a = PetriNet::new();
        net_a.add_place(Place::bridge_out("outbox", "net-b", "nonexistent"));

        let mut net_b = PetriNet::new();
        net_b.add_place(Place::bridge_in("inbox"));

        let mut resolver = MockResolver::new();
        resolver.add_net("net-a", net_a);
        resolver.add_net("net-b", net_b);

        let report = validate_all_bridges(&resolver);
        assert!(!report.is_valid);
        // Issue should be prefixed with the net ID
        assert!(report
            .issues
            .iter()
            .any(|i| i.node_id.starts_with("net-a/")));
    }

    #[test]
    fn source_net_missing_strict() {
        let mut net_b = PetriNet::new();
        net_b.add_place(Place::bridge_in_from("inbox", "net-a", "outbox"));

        let mut resolver = MockResolver::new();
        // net-a is NOT deployed
        resolver.add_net("net-b", net_b.clone());

        let report = validate_bridges("net-b", &net_b, &resolver, BridgeValidationMode::Strict);
        assert!(!report.is_valid);
        assert_eq!(report.summary.error_count, 1);
        assert_eq!(report.issues[0].code, "BRIDGE_SOURCE_NET_MISSING");
    }

    #[test]
    fn source_net_missing_warn() {
        let mut net_b = PetriNet::new();
        net_b.add_place(Place::bridge_in_from("inbox", "net-a", "outbox"));

        let mut resolver = MockResolver::new();
        resolver.add_net("net-b", net_b.clone());

        let report = validate_bridges("net-b", &net_b, &resolver, BridgeValidationMode::Warn);
        assert!(report.is_valid); // warning, not error
        assert_eq!(report.summary.warning_count, 1);
        assert_eq!(report.issues[0].code, "BRIDGE_SOURCE_NET_MISSING");
    }

    #[test]
    fn missing_target_net_with_fuzzy_suggestion() {
        let mut net_a = PetriNet::new();
        net_a.add_place(Place::bridge_out("outbox", "net-bb", "inbox"));

        let mut net_b = PetriNet::new();
        net_b.add_place(Place::bridge_in("inbox"));

        let mut resolver = MockResolver::new();
        resolver.add_net("net-a", net_a.clone());
        resolver.add_net("net-b", net_b);

        let report = validate_bridges("net-a", &net_a, &resolver, BridgeValidationMode::Strict);
        assert!(!report.is_valid);
        assert_eq!(report.issues[0].code, "BRIDGE_TARGET_NET_MISSING");
        assert!(
            report.issues[0].message.contains("Did you mean"),
            "Expected fuzzy suggestion for close net name, got: {}",
            report.issues[0].message
        );
    }

    #[test]
    fn dynamic_target_place_name_skipped() {
        let mut net_a = PetriNet::new();
        net_a.add_place(Place::bridge_out("outbox", "net-b", "$params.place"));

        let resolver = MockResolver::new();
        let report = validate_bridges("net-a", &net_a, &resolver, BridgeValidationMode::Strict);
        assert!(report.is_valid);
        assert_eq!(report.summary.info_count, 1);
        assert_eq!(report.issues[0].code, "BRIDGE_DYNAMIC_TARGET");
    }

    #[test]
    fn dynamic_bridge_in_source_skipped() {
        let mut net_b = PetriNet::new();
        net_b.add_place(Place::bridge_in_from("inbox", "$params.parent", "outbox"));

        let resolver = MockResolver::new();
        let report = validate_bridges("net-b", &net_b, &resolver, BridgeValidationMode::Strict);
        // Dynamic source annotations are silently skipped — no issues
        assert!(report.is_valid);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn reply_to_exists_no_false_positive() {
        let mut net_a = PetriNet::new();
        net_a.add_place(Place::bridge_out_reply(
            "outbox", "net-b", "inbox", "my_reply",
        ));
        net_a.add_place(Place::bridge_reply("my_reply"));

        let mut net_b = PetriNet::new();
        net_b.add_place(Place::bridge_in("inbox"));

        let mut resolver = MockResolver::new();
        resolver.add_net("net-a", net_a.clone());
        resolver.add_net("net-b", net_b);

        let report = validate_bridges("net-a", &net_a, &resolver, BridgeValidationMode::Strict);
        assert!(report.is_valid);
        assert!(!report
            .issues
            .iter()
            .any(|i| i.code == "BRIDGE_REPLY_PLACE_MISSING"));
    }

    #[test]
    fn bridge_in_without_source_annotation_no_issues() {
        let mut net_b = PetriNet::new();
        net_b.add_place(Place::bridge_in("inbox")); // no source annotation

        let resolver = MockResolver::new();
        let report = validate_bridges("net-b", &net_b, &resolver, BridgeValidationMode::Strict);
        assert!(report.is_valid);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn net_with_no_bridges_is_clean() {
        let mut net = PetriNet::new();
        net.add_place(Place::internal("start"));
        net.add_place(Place::signal("trigger"));
        net.add_place(Place::terminal("done"));

        let mut resolver = MockResolver::new();
        resolver.add_net("my-net", net.clone());

        let report = validate_bridges("my-net", &net, &resolver, BridgeValidationMode::Strict);
        assert!(report.is_valid);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn validate_all_bridges_empty_registry() {
        let resolver = MockResolver::new();
        let report = validate_all_bridges(&resolver);
        assert!(report.is_valid);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn reply_channels_missing_local_place() {
        let mut channels = HashMap::new();
        channels.insert("result".to_string(), "result_inbox".to_string());
        channels.insert("error".to_string(), "error_inbox".to_string());

        let mut net_a = PetriNet::new();
        net_a.add_place(Place::bridge_out_reply_channels(
            "outbox", "net-b", "inbox", channels,
        ));
        // Only add one of the two reply places
        net_a.add_place(Place::bridge_reply_channel("result_inbox", "result"));
        // error_inbox is missing

        let mut net_b = PetriNet::new();
        net_b.add_place(Place::bridge_in("inbox"));

        let mut resolver = MockResolver::new();
        resolver.add_net("net-a", net_a.clone());
        resolver.add_net("net-b", net_b);

        let report = validate_bridges("net-a", &net_a, &resolver, BridgeValidationMode::Strict);
        assert!(!report.is_valid);
        let reply_issues: Vec<_> = report
            .issues
            .iter()
            .filter(|i| i.code == "BRIDGE_REPLY_PLACE_MISSING")
            .collect();
        assert_eq!(reply_issues.len(), 1);
        assert!(reply_issues[0].message.contains("error_inbox"));
    }
}
