use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use mekhan_service::models::template::{
    BranchCondition, ExecutionBackendType, ExecutionSpecConfig, Port, Position, TaskBlockConfig,
    TaskStepConfig, WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};

use super::layout;

// ---------------------------------------------------------------------------
// DSL types (shared between YAML and HCL)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct DslWorkflow {
    pub steps: IndexMap<String, DslStep>,
    pub flow: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DslStep {
    #[serde(rename = "type")]
    pub step_type: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    // start
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_data: Option<serde_json::Value>,

    // human_task
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_title: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<DslTaskStep>>,

    // automated_step
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<DslExecution>,

    // decision
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<DslBranchCondition>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,

    // loop
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_condition: Option<String>,

    // scope
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DslTaskStep {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DslExecution {
    pub backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DslBranchCondition {
    pub edge: String,
    pub label: String,
    pub guard: String,
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

fn edge_id(source: &str, target: &str, handle: Option<&str>) -> String {
    match handle {
        Some(h) => format!("edge_{}_{}_to_{}", source, h, target),
        None => format!("edge_{}_to_{}", source, target),
    }
}

fn title_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
            let label = step
                .label
                .clone()
                .unwrap_or_else(|| title_case(key));
            let data = step_to_node_data(key, step, &label)?;
            let (width, height) = if step.step_type == "scope" {
                (step.width.or(Some(400.0)), step.height.or(Some(300.0)))
            } else {
                (None, None)
            };
            nodes.push(WorkflowNode {
                id: key.clone(),
                node_type: step.step_type.clone(),
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
                    edge_type: "sequence".to_string(),
                });
            }
        }

        let mut graph = WorkflowGraph {
            nodes,
            edges,
            viewport: None,
        };

        // Apply decision edge labels from conditions
        apply_decision_edge_labels(&mut graph, &self.steps);

        // Auto-layout
        layout::auto_layout(&mut graph);

        Ok(graph)
    }
}

fn step_to_node_data(
    key: &str,
    step: &DslStep,
    label: &str,
) -> Result<WorkflowNodeData, String> {
    match step.step_type.as_str() {
        "start" => Ok(WorkflowNodeData::Start {
            label: label.to_string(),
            description: step.description.clone(),
            // DSL still carries `initial_data` for read-compat with old files;
            // the typed-ports model expects a `Port` here. CLI DSL doesn't yet
            // express ports, so we default to an empty input port. Round-trip
            // through DSL is lossy for typed Start ports until the DSL format
            // gains a `initial` schema.
            initial: Port::empty_input(),
        }),
        "end" => Ok(WorkflowNodeData::End {
            label: label.to_string(),
            description: step.description.clone(),
            terminal: mekhan_service::models::template::default_terminal_port(),
        }),
        "human_task" => {
            let task_steps = step
                .steps
                .as_ref()
                .map(|dsl_steps| {
                    dsl_steps
                        .iter()
                        .enumerate()
                        .map(|(i, ds)| {
                            let blocks: Vec<TaskBlockConfig> = ds
                                .blocks
                                .as_ref()
                                .map(|b| {
                                    b.iter()
                                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                                        .collect()
                                })
                                .unwrap_or_default();
                            TaskStepConfig {
                                id: format!("{}-step-{}", key, i),
                                title: ds.title.clone(),
                                description_mdsvex: ds.description.clone(),
                                blocks,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();

            Ok(WorkflowNodeData::HumanTask {
                label: label.to_string(),
                description: step.description.clone(),
                task_title: step
                    .task_title
                    .clone()
                    .unwrap_or_else(|| label.to_string()),
                instructions_mdsvex: step.instructions.clone(),
                steps: task_steps,
            })
        }
        "automated_step" => {
            let exec = step.execution.as_ref().ok_or_else(|| {
                format!("automated_step '{}' requires an 'execution' field", key)
            })?;
            // Merge entrypoint and files list into config
            let mut config = exec.config.clone();
            if let serde_json::Value::Object(ref mut map) = config {
                if let Some(ref ep) = exec.entrypoint {
                    map.insert("entrypoint".to_string(), serde_json::Value::String(ep.clone()));
                }
                if !exec.files.is_empty() {
                    let files_arr: Vec<serde_json::Value> = exec.files.iter()
                        .map(|f| serde_json::Value::String(f.clone()))
                        .collect();
                    map.insert("required_files".to_string(), serde_json::Value::Array(files_arr));
                }
            }
            // Parse the backend discriminator via serde — keeps the DSL's
            // accepted value set in lockstep with the wire enum.
            let backend_type: ExecutionBackendType = serde_json::from_value(
                serde_json::Value::String(exec.backend.clone()),
            )
            .map_err(|_| {
                format!(
                    "automated_step '{}' has unknown backend '{}' (expected one of: python, process, docker, http, llm, file_ops, kreuzberg)",
                    key, exec.backend
                )
            })?;
            Ok(WorkflowNodeData::AutomatedStep {
                label: label.to_string(),
                description: step.description.clone(),
                execution_spec: ExecutionSpecConfig {
                    backend_type,
                    entrypoint: None,
                    config,
                },
                input: Port::empty_input(),
                output: mekhan_service::models::template::default_output_port(backend_type),
                retry_policy: Default::default(),
            })
        }
        "decision" => {
            let dsl_conditions = step.conditions.as_ref().cloned().unwrap_or_default();
            let conditions: Vec<BranchCondition> = dsl_conditions
                .iter()
                .map(|dc| {
                    let eid = edge_id(
                        key,
                        &dc.edge,
                        Some(&dc.label.to_lowercase().replace(' ', "_")),
                    );
                    BranchCondition {
                        edge_id: eid,
                        label: dc.label.clone(),
                        guard: dc.guard.clone(),
                    }
                })
                .collect();

            let default_branch = step.default_branch.as_ref().map(|target| {
                edge_id(key, target, None)
            });

            Ok(WorkflowNodeData::Decision {
                label: label.to_string(),
                description: step.description.clone(),
                conditions,
                default_branch,
            })
        }
        "parallel_split" => Ok(WorkflowNodeData::ParallelSplit {
            label: label.to_string(),
            description: step.description.clone(),
        }),
        "parallel_join" => Ok(WorkflowNodeData::ParallelJoin {
            label: label.to_string(),
            description: step.description.clone(),
            merge_strategy: Default::default(),
        }),
        "loop" => {
            let max_iter = step.max_iterations.ok_or_else(|| {
                format!("loop '{}' requires 'max_iterations'", key)
            })?;
            let condition = step.loop_condition.clone().ok_or_else(|| {
                format!("loop '{}' requires 'loop_condition'", key)
            })?;
            Ok(WorkflowNodeData::Loop {
                label: label.to_string(),
                description: step.description.clone(),
                max_iterations: max_iter,
                loop_condition: condition,
            })
        }
        "scope" => Ok(WorkflowNodeData::Scope {
            label: label.to_string(),
            description: step.description.clone(),
        }),
        other => Err(format!("unknown step type '{}' for step '{}'", other, key)),
    }
}

fn apply_decision_edge_labels(
    graph: &mut WorkflowGraph,
    steps: &IndexMap<String, DslStep>,
) {
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
            steps.insert(node.id.clone(), node_to_dsl_step(node));
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

fn node_to_dsl_step(node: &WorkflowNode) -> DslStep {
    let mut step = DslStep {
        step_type: node.node_type.clone(),
        label: Some(node.data.label().to_string()),
        description: node.data.description().map(|s| s.to_string()),
        initial_data: None,
        task_title: None,
        instructions: None,
        steps: None,
        execution: None,
        conditions: None,
        default_branch: None,
        max_iterations: None,
        loop_condition: None,
        children: Vec::new(),
        width: node.width,
        height: node.height,
    };

    match &node.data {
        WorkflowNodeData::Start { .. } => {
            // DSL doesn't yet express typed Start ports; the round-trip drops
            // the declared `initial` port shape. CLI DSL is dev tooling — when
            // the format gains a `initial` schema (Phase 4-ish), populate it
            // here.
        }
        WorkflowNodeData::End { .. } => {}
        WorkflowNodeData::HumanTask {
            task_title,
            instructions_mdsvex,
            steps: task_steps,
            ..
        } => {
            step.task_title = Some(task_title.clone());
            step.instructions = instructions_mdsvex.clone();
            if !task_steps.is_empty() {
                step.steps = Some(
                    task_steps
                        .iter()
                        .map(|ts| DslTaskStep {
                            title: ts.title.clone(),
                            description: ts.description_mdsvex.clone(),
                            blocks: if ts.blocks.is_empty() {
                                None
                            } else {
                                Some(
                                    ts.blocks
                                        .iter()
                                        .filter_map(|b| serde_json::to_value(b).ok())
                                        .collect(),
                                )
                            },
                        })
                        .collect(),
                );
            }
        }
        WorkflowNodeData::AutomatedStep {
            execution_spec, ..
        } => {
            // Extract entrypoint and files from config into their own fields
            let mut config = execution_spec.config.clone();
            let (entrypoint, files) = if let serde_json::Value::Object(ref mut map) = config {
                let ep = map.remove("entrypoint")
                    .and_then(|v| v.as_str().map(|s| s.to_string()));
                let f = map.remove("required_files")
                    .and_then(|v| v.as_array().map(|arr| {
                        arr.iter()
                            .filter_map(|item| item.as_str().map(|s| s.to_string()))
                            .collect()
                    }))
                    .unwrap_or_default();
                (ep, f)
            } else {
                (None, vec![])
            };
            // Round-trip the enum through serde to recover the canonical
            // snake_case wire string (`python`, `file_ops`, …) so the DSL
            // export matches what users would type into YAML/HCL.
            let backend = serde_json::to_value(execution_spec.backend_type)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            step.execution = Some(DslExecution {
                backend,
                entrypoint,
                files,
                config,
            });
        }
        WorkflowNodeData::Decision {
            conditions,
            default_branch,
            ..
        } => {
            if !conditions.is_empty() {
                step.conditions = Some(
                    conditions
                        .iter()
                        .map(|bc| {
                            // Extract target from edge_id: edge_source_handle_to_TARGET
                            let target = extract_edge_target(&bc.edge_id);
                            DslBranchCondition {
                                edge: target,
                                label: bc.label.clone(),
                                guard: bc.guard.clone(),
                            }
                        })
                        .collect(),
                );
            }
            if let Some(db) = default_branch {
                step.default_branch = Some(extract_edge_target(db));
            }
        }
        WorkflowNodeData::ParallelSplit { .. } => {}
        WorkflowNodeData::ParallelJoin { .. } => {}
        WorkflowNodeData::Scope { .. } => {
            // children are populated by from_workflow_graph after building the step map
        }
        WorkflowNodeData::Loop {
            max_iterations,
            loop_condition,
            ..
        } => {
            step.max_iterations = Some(*max_iterations);
            step.loop_condition = Some(loop_condition.clone());
        }
        WorkflowNodeData::Trigger { .. } => {
            // DSL doesn't yet model triggers — they're declared in the GUI for
            // now. Round-trip through JSON would lose the trigger; this exit
            // simply drops them, matching how legacy DSL templates behave.
        }
    }

    step
}

/// Extract the target step key from an auto-generated edge ID.
/// e.g., "edge_check_yes_to_process" → "process"
fn extract_edge_target(edge_id: &str) -> String {
    if let Some(pos) = edge_id.rfind("_to_") {
        edge_id[pos + 4..].to_string()
    } else {
        edge_id.to_string()
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
                .filter(|(t, h)| {
                    h.is_none() && !used_edges.contains(&(current, t, None))
                })
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
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::Start {
                        label: "Start".to_string(),
                        description: None,
                        initial: Port::empty_input(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "container".to_string(),
                    node_type: "scope".to_string(),
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
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::End {
                        label: "Done".to_string(),
                        description: None,
                    terminal: mekhan_service::models::template::default_terminal_port(),
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
                edge_type: "sequence".to_string(),
            }],
            viewport: None,
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
            if let TaskBlockConfig::Image { filenames, display, .. } = &steps[0].blocks[0] {
                assert_eq!(filenames, &["photo1.png", "photo2.jpg"]);
                assert_eq!(*display, mekhan_service::models::template::ImageDisplay::Grid);
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
        use mekhan_service::models::template::{
            DownloadItemConfig, ImageDisplay, TaskStepConfig,
        };

        let graph = WorkflowGraph {
            nodes: vec![
                WorkflowNode {
                    id: "start".to_string(),
                    node_type: "start".to_string(),
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::Start {
                        label: "Start".to_string(),
                        description: None,
                        initial: Port::empty_input(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "review".to_string(),
                    node_type: "human_task".to_string(),
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
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "done".to_string(),
                    node_type: "end".to_string(),
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::End {
                        label: "Done".to_string(),
                        description: None,
                        terminal: mekhan_service::models::template::default_terminal_port(),
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
                    edge_type: "sequence".to_string(),
                },
                WorkflowEdge {
                    id: "e2".to_string(),
                    source: "review".to_string(),
                    target: "done".to_string(),
                    source_handle: None,
                    target_handle: Some("in".to_string()),
                    label: None,
                    edge_type: "sequence".to_string(),
                },
            ],
            viewport: None,
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
                    assert_eq!(alt.as_deref(), Some("Uploaded invoice"), "[{via}] image alt");
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
        let dsl_back: DslWorkflow =
            serde_yaml_ng::from_str(&yaml).expect("parse dsl yaml back");
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
}
