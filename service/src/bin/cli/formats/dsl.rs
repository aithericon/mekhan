use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use mekhan_service::models::template::{
    Position, WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};

use super::layout;

// ---------------------------------------------------------------------------
// DSL types (shared between YAML and HCL)
//
// The per-node payload types and the node<->step mapping are single-sourced
// in `mekhan_service::models::template::dsl`, next to `WorkflowNodeData`, so
// the model<->DSL match is compiler-checked. Re-exported here so `hcl.rs` /
// `yaml.rs` keep importing them via `super::dsl::*` unchanged.
// ---------------------------------------------------------------------------

pub use mekhan_service::models::template::dsl::{
    edge_id, title_case, DslBranchCondition, DslExecution, DslStep, DslTaskStep,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct DslWorkflow {
    pub steps: IndexMap<String, DslStep>,
    pub flow: Vec<String>,
}

// ---------------------------------------------------------------------------
// Flow parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ParsedEdge {
    pub source: String,
    pub target: String,
    pub source_handle: Option<String>,
}

pub fn parse_flow_entry(entry: &str) -> Result<Vec<ParsedEdge>, String> {
    let parts: Vec<&str> = entry.split("->").map(str::trim).collect();
    if parts.len() < 2 {
        return Err(format!(
            "flow entry must have at least two steps: '{}'",
            entry
        ));
    }

    let mut edges = Vec::new();
    for window in parts.windows(2) {
        let (source, source_handle) = parse_step_ref(window[0])?;
        let (target, _) = parse_step_ref(window[1])?;
        edges.push(ParsedEdge {
            source,
            target,
            source_handle,
        });
    }
    Ok(edges)
}

fn parse_step_ref(s: &str) -> Result<(String, Option<String>), String> {
    if let Some(bracket_pos) = s.find('[') {
        if !s.ends_with(']') {
            return Err(format!("malformed step reference: '{}'", s));
        }
        let key = s[..bracket_pos].to_string();
        let handle = s[bracket_pos + 1..s.len() - 1].to_string();
        if key.is_empty() || handle.is_empty() {
            return Err(format!("empty step key or handle in: '{}'", s));
        }
        Ok((key, Some(handle)))
    } else {
        Ok((s.to_string(), None))
    }
}

// ---------------------------------------------------------------------------
// DSL → WorkflowGraph
// ---------------------------------------------------------------------------

impl DslWorkflow {
    pub fn to_workflow_graph(&self) -> Result<WorkflowGraph, String> {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        // Convert steps to nodes
        for (key, step) in &self.steps {
            let label = step.label.clone().unwrap_or_else(|| title_case(key));
            let data = WorkflowNodeData::from_dsl_step(key, step, &label)?;
            let (width, height) = if step.step_type == "scope" {
                (step.width.or(Some(400.0)), step.height.or(Some(300.0)))
            } else {
                (None, None)
            };
            nodes.push(WorkflowNode {
                id: key.clone(),
                node_type: step.step_type.clone(),
                slug: None,
                position: Position { x: 0.0, y: 0.0 },
                data,
                parent_id: None,
                width,
                height,
            });
        }

        // Set parent_id on children of scope nodes
        for (key, step) in &self.steps {
            if step.step_type == "scope" {
                for child_key in &step.children {
                    if let Some(child) = nodes.iter_mut().find(|n| n.id == *child_key) {
                        child.parent_id = Some(key.clone());
                    } else {
                        return Err(format!(
                            "scope '{}' references unknown child step '{}'",
                            key, child_key
                        ));
                    }
                }
            }
        }

        // Parse flow entries into edges
        let mut seen: HashSet<String> = HashSet::new();
        for entry in &self.flow {
            let parsed = parse_flow_entry(entry)?;
            for pe in parsed {
                if !self.steps.contains_key(&pe.source) {
                    return Err(format!("flow references unknown step '{}'", pe.source));
                }
                if !self.steps.contains_key(&pe.target) {
                    return Err(format!("flow references unknown step '{}'", pe.target));
                }

                let eid = edge_id(&pe.source, &pe.target, pe.source_handle.as_deref());
                if seen.contains(&eid) {
                    continue;
                }
                seen.insert(eid.clone());

                edges.push(WorkflowEdge {
                    id: eid,
                    source: pe.source,
                    target: pe.target,
                    source_handle: pe.source_handle,
                    target_handle: Some("in".to_string()),
                    label: None,
                    join: None,
                    edge_type: "sequence".to_string(),
                });
            }
        }

        let mut graph = WorkflowGraph {
            nodes,
            edges,
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: None,
        };

        // Apply decision edge labels from conditions
        apply_decision_edge_labels(&mut graph, &self.steps);

        // Auto-layout
        layout::auto_layout(&mut graph);

        Ok(graph)
    }
}

fn apply_decision_edge_labels(graph: &mut WorkflowGraph, steps: &IndexMap<String, DslStep>) {
    for (key, step) in steps {
        if step.step_type != "decision" {
            continue;
        }
        if let Some(conditions) = &step.conditions {
            for dc in conditions {
                let handle = dc.label.to_lowercase().replace(' ', "_");
                let eid = edge_id(key, &dc.edge, Some(&handle));
                if let Some(edge) = graph.edges.iter_mut().find(|e| e.id == eid) {
                    edge.label = Some(dc.label.clone());
                    edge.source_handle = Some(handle);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// WorkflowGraph → DSL
// ---------------------------------------------------------------------------

impl DslWorkflow {
    pub fn from_workflow_graph(graph: &WorkflowGraph) -> Self {
        let mut steps = IndexMap::new();
        for node in &graph.nodes {
            steps.insert(node.id.clone(), node.data.to_dsl_step(node));
        }

        // Populate scope children from parent_id references
        for node in &graph.nodes {
            if let Some(ref pid) = node.parent_id {
                if let Some(parent_step) = steps.get_mut(pid) {
                    parent_step.children.push(node.id.clone());
                }
            }
        }

        let flow = build_flow_chains(&graph.edges);

        DslWorkflow { steps, flow }
    }
}

/// Build compact flow chain strings from a list of edges.
fn build_flow_chains(edges: &[WorkflowEdge]) -> Vec<String> {
    if edges.is_empty() {
        return vec![];
    }

    // Build adjacency: source -> [(target, handle)]
    let mut adj: HashMap<&str, Vec<(&str, Option<&str>)>> = HashMap::new();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();

    for edge in edges {
        adj.entry(edge.source.as_str())
            .or_default()
            .push((edge.target.as_str(), edge.source_handle.as_deref()));
        *in_degree.entry(edge.target.as_str()).or_default() += 1;
        in_degree.entry(edge.source.as_str()).or_default();
    }

    let mut used_edges: HashSet<(&str, &str, Option<&str>)> = HashSet::new();
    let mut chains = Vec::new();

    // Start chains from nodes with in_degree == 0 or that are sources of handled edges
    let mut chain_starts: Vec<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&k, _)| k)
        .collect();
    chain_starts.sort();

    for &start in &chain_starts {
        let mut chain = vec![format_step_ref(start, None)];
        let mut current = start;

        loop {
            let targets = adj.get(current).cloned().unwrap_or_default();
            // Follow simple (no handle) edges where the target has only 1 incoming edge
            let simple: Vec<_> = targets
                .iter()
                .filter(|(t, h)| h.is_none() && !used_edges.contains(&(current, t, None)))
                .collect();

            if simple.len() == 1 {
                let (target, _) = simple[0];
                used_edges.insert((current, target, None));
                chain.push(format_step_ref(target, None));
                current = target;
            } else {
                break;
            }
        }

        if chain.len() >= 2 {
            chains.push(chain.join(" -> "));
        }
    }

    // Emit remaining unused edges individually
    for edge in edges {
        let key = (
            edge.source.as_str(),
            edge.target.as_str(),
            edge.source_handle.as_deref(),
        );
        if !used_edges.contains(&key) {
            used_edges.insert(key);
            let entry = format!(
                "{} -> {}",
                format_step_ref(&edge.source, edge.source_handle.as_deref()),
                format_step_ref(&edge.target, None),
            );
            chains.push(entry);
        }
    }

    chains
}

fn format_step_ref(key: &str, handle: Option<&str>) -> String {
    match handle {
        Some(h) => format!("{}[{}]", key, h),
        None => key.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mekhan_service::models::template::{Port, TaskBlockConfig};

    #[test]
    fn scope_step_sets_parent_id_on_children() {
        let yaml = r#"
steps:
  start:
    type: start
  my_scope:
    type: scope
    label: "Approval"
    children:
      - review
    width: 500
    height: 400
  review:
    type: human_task
    task_title: "Review"
    steps: []
  done:
    type: end
flow:
  - start -> review -> done
"#;
        let dsl: DslWorkflow = serde_yaml_ng::from_str(yaml).unwrap();
        let graph = dsl.to_workflow_graph().unwrap();

        // Scope node should have width/height
        let scope_node = graph.nodes.iter().find(|n| n.id == "my_scope").unwrap();
        assert_eq!(scope_node.width, Some(500.0));
        assert_eq!(scope_node.height, Some(400.0));
        assert_eq!(scope_node.parent_id, None);

        // Child should have parent_id pointing to scope
        let review_node = graph.nodes.iter().find(|n| n.id == "review").unwrap();
        assert_eq!(review_node.parent_id, Some("my_scope".to_string()));
    }

    #[test]
    fn scope_roundtrip_via_dsl() {
        // Build a graph with scope manually
        let graph = WorkflowGraph {
            nodes: vec![
                WorkflowNode {
                    id: "start".to_string(),
                    node_type: "start".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::Start {
                        label: "Start".to_string(),
                        description: None,
                        initial: Port::empty_input(),
                        process_name: None,
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "container".to_string(),
                    node_type: "scope".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::Scope {
                        label: "Container".to_string(),
                        description: Some("Groups tasks".to_string()),
                    },
                    parent_id: None,
                    width: Some(600.0),
                    height: Some(400.0),
                },
                WorkflowNode {
                    id: "task1".to_string(),
                    node_type: "end".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::End {
                        label: "Done".to_string(),
                        description: None,
                        terminal: mekhan_service::models::template::default_terminal_port(),
                        result_mapping: Vec::new(),
                    },
                    parent_id: Some("container".to_string()),
                    width: None,
                    height: None,
                },
            ],
            edges: vec![WorkflowEdge {
                id: "e1".to_string(),
                source: "start".to_string(),
                target: "task1".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                join: None,
                edge_type: "sequence".to_string(),
            }],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: None,
        };

        // Convert to DSL and back
        let dsl = DslWorkflow::from_workflow_graph(&graph);

        // Check DSL captured children
        let scope_step = dsl.steps.get("container").unwrap();
        assert_eq!(scope_step.step_type, "scope");
        assert_eq!(scope_step.children, vec!["task1"]);
        assert_eq!(scope_step.width, Some(600.0));
        assert_eq!(scope_step.height, Some(400.0));

        // Roundtrip back
        let graph2 = dsl.to_workflow_graph().unwrap();
        let child = graph2.nodes.iter().find(|n| n.id == "task1").unwrap();
        assert_eq!(child.parent_id, Some("container".to_string()));

        let scope = graph2.nodes.iter().find(|n| n.id == "container").unwrap();
        assert_eq!(scope.width, Some(600.0));
        assert_eq!(scope.height, Some(400.0));
    }

    #[test]
    fn scope_invalid_child_fails() {
        let yaml = r#"
steps:
  start:
    type: start
  my_scope:
    type: scope
    children:
      - nonexistent
  end:
    type: end
flow:
  - start -> end
"#;
        let dsl: DslWorkflow = serde_yaml_ng::from_str(yaml).unwrap();
        let result = dsl.to_workflow_graph();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown child step"));
    }

    #[test]
    fn image_block_in_human_task_yaml() {
        let yaml = r#"
steps:
  start:
    type: start
  review:
    type: human_task
    task_title: "Photo Review"
    steps:
      - title: "Check Photos"
        blocks:
          - type: image
            filenames:
              - photo1.png
              - photo2.jpg
            display: grid
          - type: file
            filename: report.pdf
  done:
    type: end
flow:
  - start -> review -> done
"#;
        let dsl: DslWorkflow = serde_yaml_ng::from_str(yaml).unwrap();
        let graph = dsl.to_workflow_graph().unwrap();

        let review = graph.nodes.iter().find(|n| n.id == "review").unwrap();
        if let WorkflowNodeData::HumanTask { steps, .. } = &review.data {
            assert_eq!(steps.len(), 1);
            assert_eq!(steps[0].blocks.len(), 2);

            // Verify image block
            if let TaskBlockConfig::Image {
                filenames, display, ..
            } = &steps[0].blocks[0]
            {
                assert_eq!(filenames, &["photo1.png", "photo2.jpg"]);
                assert_eq!(
                    *display,
                    mekhan_service::models::template::ImageDisplay::Grid
                );
            } else {
                panic!("expected Image block, got {:?}", steps[0].blocks[0]);
            }

            // Verify file block
            if let TaskBlockConfig::File { filename } = &steps[0].blocks[1] {
                assert_eq!(filename, "report.pdf");
            } else {
                panic!("expected File block, got {:?}", steps[0].blocks[1]);
            }
        } else {
            panic!("expected HumanTask");
        }
    }

    /// The DSL/HCL formatters convert task blocks via serde (`to_value` on
    /// emit, `from_value` on parse), so the additive `url`/`alt`/`caption`
    /// fields on image and the new `download` variant must survive a full
    /// round-trip through both formats — including unresolved `{{ ... }}`
    /// interpolation placeholders, which are just opaque strings to the CLI.
    #[test]
    fn url_image_and_download_blocks_roundtrip_dsl_and_hcl() {
        use mekhan_service::models::template::{DownloadItemConfig, ImageDisplay, TaskStepConfig};

        let graph = WorkflowGraph {
            nodes: vec![
                WorkflowNode {
                    id: "start".to_string(),
                    node_type: "start".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::Start {
                        label: "Start".to_string(),
                        description: None,
                        initial: Port::empty_input(),
                        process_name: None,
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "review".to_string(),
                    node_type: "human_task".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::HumanTask {
                        label: "Review".to_string(),
                        description: None,
                        task_title: "Review Invoice".to_string(),
                        instructions_mdsvex: None,
                        steps: vec![TaskStepConfig {
                            id: "step-verify".to_string(),
                            title: "Verify".to_string(),
                            description_mdsvex: None,
                            blocks: vec![
                                TaskBlockConfig::Image {
                                    filenames: vec![],
                                    display: ImageDisplay::Single,
                                    url: Some("{{ invoice_file.url }}".to_string()),
                                    alt: Some("Uploaded invoice".to_string()),
                                    caption: Some("Original document".to_string()),
                                },
                                TaskBlockConfig::Download {
                                    downloads: vec![DownloadItemConfig {
                                        url: "{{ invoice_file.url }}".to_string(),
                                        filename: "{{ invoice_file.filename }}".to_string(),
                                        size: None,
                                        mime_type: Some(
                                            "{{ invoice_file.content_type }}".to_string(),
                                        ),
                                        thumbnail_url: None,
                                        description: Some("Original uploaded invoice".to_string()),
                                    }],
                                },
                            ],
                        }],
                        steps_ref: None,
                        capacity: None,
                        requirements: None,
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "done".to_string(),
                    node_type: "end".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::End {
                        label: "Done".to_string(),
                        description: None,
                        terminal: mekhan_service::models::template::default_terminal_port(),
                        result_mapping: Vec::new(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
            ],
            edges: vec![
                WorkflowEdge {
                    id: "e1".to_string(),
                    source: "start".to_string(),
                    target: "review".to_string(),
                    source_handle: None,
                    target_handle: Some("in".to_string()),
                    label: None,
                    join: None,
                    edge_type: "sequence".to_string(),
                },
                WorkflowEdge {
                    id: "e2".to_string(),
                    source: "review".to_string(),
                    target: "done".to_string(),
                    source_handle: None,
                    target_handle: Some("in".to_string()),
                    label: None,
                    join: None,
                    edge_type: "sequence".to_string(),
                },
            ],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: None,
        };

        // Assert the url image + download blocks survived a round-trip,
        // placeholders intact.
        fn assert_blocks_intact(graph: &WorkflowGraph, via: &str) {
            let review = graph
                .nodes
                .iter()
                .find(|n| n.id == "review")
                .unwrap_or_else(|| panic!("[{via}] review node missing after round-trip"));
            let WorkflowNodeData::HumanTask { steps, .. } = &review.data else {
                panic!("[{via}] expected HumanTask, got {:?}", review.data);
            };
            assert_eq!(steps.len(), 1, "[{via}] step count");
            assert_eq!(steps[0].blocks.len(), 2, "[{via}] block count");

            match &steps[0].blocks[0] {
                TaskBlockConfig::Image {
                    url, alt, caption, ..
                } => {
                    assert_eq!(
                        url.as_deref(),
                        Some("{{ invoice_file.url }}"),
                        "[{via}] image url placeholder lost"
                    );
                    assert_eq!(
                        alt.as_deref(),
                        Some("Uploaded invoice"),
                        "[{via}] image alt"
                    );
                    assert_eq!(
                        caption.as_deref(),
                        Some("Original document"),
                        "[{via}] image caption"
                    );
                }
                other => panic!("[{via}] expected Image block, got {other:?}"),
            }

            match &steps[0].blocks[1] {
                TaskBlockConfig::Download { downloads } => {
                    assert_eq!(downloads.len(), 1, "[{via}] download item count");
                    let d = &downloads[0];
                    assert_eq!(d.url, "{{ invoice_file.url }}", "[{via}] download url");
                    assert_eq!(
                        d.filename, "{{ invoice_file.filename }}",
                        "[{via}] download filename placeholder lost"
                    );
                    assert_eq!(
                        d.mime_type.as_deref(),
                        Some("{{ invoice_file.content_type }}"),
                        "[{via}] download mime_type"
                    );
                    assert_eq!(
                        d.description.as_deref(),
                        Some("Original uploaded invoice"),
                        "[{via}] download description"
                    );
                }
                other => panic!("[{via}] expected Download block, got {other:?}"),
            }
        }

        // DSL: graph -> DslWorkflow -> YAML string -> DslWorkflow -> graph
        let dsl = DslWorkflow::from_workflow_graph(&graph);
        let yaml = serde_yaml_ng::to_string(&dsl).expect("serialize dsl yaml");
        assert!(
            yaml.contains("{{ invoice_file.url }}"),
            "DSL yaml should carry the raw placeholder, got:\n{yaml}"
        );
        let dsl_back: DslWorkflow = serde_yaml_ng::from_str(&yaml).expect("parse dsl yaml back");
        let graph_dsl = dsl_back.to_workflow_graph().expect("dsl -> graph");
        assert_blocks_intact(&graph_dsl, "dsl");

        // HCL: graph -> HCL string -> graph
        let hcl_str = super::super::hcl::emit(&graph).expect("emit hcl");
        assert!(
            hcl_str.contains("{{ invoice_file.url }}"),
            "HCL should carry the raw placeholder, got:\n{hcl_str}"
        );
        let graph_hcl = super::super::hcl::parse(&hcl_str).expect("parse hcl back");
        assert_blocks_intact(&graph_hcl, "hcl");
    }

    /// `Start.initial` (typed port), `Start.process_name`, and
    /// `AutomatedStep.retry_policy` historically dropped silently on a
    /// graph -> DSL -> graph round-trip. They now have dedicated DSL fields
    /// and must survive a full round-trip through `to/from_dsl_step`, YAML,
    /// and HCL. Non-default values on every field so a "default fallback"
    /// can't pass by accident.
    #[test]
    fn start_ports_and_retry_policy_roundtrip_all_formats() {
        use mekhan_service::models::template::{
            BackoffKind, ExecutionBackendType, ExecutionSpecConfig, FieldKind, PortField,
            RetryPolicy,
        };

        let custom_initial = Port {
            id: "in".to_string(),
            label: "Order Input".to_string(),
            fields: vec![
                PortField {
                    schema: None,
                    name: "order_id".to_string(),
                    label: "Order ID".to_string(),
                    kind: FieldKind::Text,
                    required: true,
                    options: None,
                    description: Some("the order".to_string()),
                    accept: None,
                },
                PortField {
                    schema: None,
                    name: "amount".to_string(),
                    label: "Amount".to_string(),
                    kind: FieldKind::Number,
                    required: false,
                    options: None,
                    description: None,
                    accept: None,
                },
            ],
        };
        let custom_retry = RetryPolicy {
            max_retries: 7,
            backoff: BackoffKind::Exponential,
            base_delay_ms: 2500,
        };
        // Sanity: every value differs from the historical default so a
        // silent drop -> default would fail the assertions below.
        assert_ne!(custom_retry, RetryPolicy::default());
        assert_ne!(
            custom_initial.fields.len(),
            Port::empty_input().fields.len()
        );

        let graph = WorkflowGraph {
            nodes: vec![
                WorkflowNode {
                    id: "start".to_string(),
                    node_type: "start".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::Start {
                        label: "Start".to_string(),
                        description: None,
                        initial: custom_initial.clone(),
                        process_name: Some("Order {{ order_id }}".to_string()),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "run".to_string(),
                    node_type: "automated_step".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::AutomatedStep {
                        label: "Run".to_string(),
                        description: None,
                        execution_spec: ExecutionSpecConfig {
                            backend_type: ExecutionBackendType::Python,
                            entrypoint: None,
                            config: serde_json::json!({ "code": "print(1)" }),
                        },
                        input: Port::empty_input(),
                        output: mekhan_service::models::template::default_output_port(
                            ExecutionBackendType::Python,
                        ),
                        retry_policy: custom_retry,
                        deployment_model: Default::default(),
                        channels: Vec::new(),
                        requirements: None,
                        asset_bindings: Vec::new(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "done".to_string(),
                    node_type: "end".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::End {
                        label: "Done".to_string(),
                        description: None,
                        terminal: mekhan_service::models::template::default_terminal_port(),
                        result_mapping: Vec::new(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
            ],
            edges: vec![
                WorkflowEdge {
                    id: "e1".to_string(),
                    source: "start".to_string(),
                    target: "run".to_string(),
                    source_handle: None,
                    target_handle: Some("in".to_string()),
                    label: None,
                    join: None,
                    edge_type: "sequence".to_string(),
                },
                WorkflowEdge {
                    id: "e2".to_string(),
                    source: "run".to_string(),
                    target: "done".to_string(),
                    source_handle: None,
                    target_handle: Some("in".to_string()),
                    label: None,
                    join: None,
                    edge_type: "sequence".to_string(),
                },
            ],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: None,
        };

        fn assert_fields_intact(
            graph: &WorkflowGraph,
            via: &str,
            expected_initial: &Port,
            expected_retry: &RetryPolicy,
        ) {
            let start = graph
                .nodes
                .iter()
                .find(|n| n.id == "start")
                .unwrap_or_else(|| panic!("[{via}] start node missing"));
            let WorkflowNodeData::Start {
                initial,
                process_name,
                ..
            } = &start.data
            else {
                panic!("[{via}] expected Start, got {:?}", start.data);
            };
            assert_eq!(
                process_name.as_deref(),
                Some("Order {{ order_id }}"),
                "[{via}] process_name lost"
            );
            assert_eq!(initial.id, expected_initial.id, "[{via}] initial.id");
            assert_eq!(
                initial.label, expected_initial.label,
                "[{via}] initial.label"
            );
            assert_eq!(
                initial.fields.len(),
                expected_initial.fields.len(),
                "[{via}] initial.fields count"
            );
            for (got, want) in initial.fields.iter().zip(&expected_initial.fields) {
                assert_eq!(got.name, want.name, "[{via}] field name");
                assert_eq!(
                    std::mem::discriminant(&got.kind),
                    std::mem::discriminant(&want.kind),
                    "[{via}] field kind for {}",
                    want.name
                );
                assert_eq!(got.required, want.required, "[{via}] field required");
                assert_eq!(
                    got.description, want.description,
                    "[{via}] field description"
                );
            }

            let run = graph
                .nodes
                .iter()
                .find(|n| n.id == "run")
                .unwrap_or_else(|| panic!("[{via}] run node missing"));
            let WorkflowNodeData::AutomatedStep { retry_policy, .. } = &run.data else {
                panic!("[{via}] expected AutomatedStep, got {:?}", run.data);
            };
            assert_eq!(
                retry_policy, expected_retry,
                "[{via}] retry_policy not round-tripped"
            );
        }

        // 1. Direct to/from_dsl_step (no serialization).
        let dsl = DslWorkflow::from_workflow_graph(&graph);
        let graph_direct = dsl.to_workflow_graph().expect("dsl -> graph");
        assert_fields_intact(
            &graph_direct,
            "to/from_dsl_step",
            &custom_initial,
            &custom_retry,
        );

        // 2. YAML round-trip.
        let yaml = serde_yaml_ng::to_string(&dsl).expect("serialize yaml");
        let dsl_yaml: DslWorkflow = serde_yaml_ng::from_str(&yaml).expect("parse yaml back");
        let graph_yaml = dsl_yaml.to_workflow_graph().expect("yaml dsl -> graph");
        assert_fields_intact(&graph_yaml, "yaml", &custom_initial, &custom_retry);

        // 3. HCL round-trip.
        let hcl_str = super::super::hcl::emit(&graph).expect("emit hcl");
        let graph_hcl = super::super::hcl::parse(&hcl_str).expect("parse hcl back");
        assert_fields_intact(&graph_hcl, "hcl", &custom_initial, &custom_retry);
    }
}
