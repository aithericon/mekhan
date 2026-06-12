use super::{
    default_join_output_port, default_max_turns, default_output_port, default_terminal_port,
    BranchCondition, ContextStrategy, DeploymentModel, ExecutionBackendType, ExecutionSpecConfig,
    JoinMode, LoopAccumulator, MergeStrategy, ModelRef, Port, RetryPolicy, TaskBlockConfig,
    TaskStepConfig, ToolErrorPolicy, WorkflowNode, WorkflowNodeData,
};
use serde::{Deserialize, Serialize};

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

    /// Declared Start input-port shape. `None` means the step omitted it
    /// (legacy DSL files), in which case `from_dsl_step` falls back to the
    /// empty-input default — preserving prior behaviour. Round-trips the
    /// typed `initial` port that GUI-authored Starts carry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial: Option<Port>,

    /// Optional Start process-name template (see
    /// `WorkflowNodeData::Start::process_name`). `None`/absent means no
    /// named-process registration, matching the historical DSL default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,

    // human_task
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_title: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<DslTaskStep>>,

    /// Opt-in dynamic form: runtime `<slug>.<field>` ref for the step list.
    #[serde(rename = "stepsRef", default, skip_serializing_if = "Option::is_none")]
    pub steps_ref: Option<String>,

    // automated_step
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<DslExecution>,

    // agent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<DslAgent>,

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

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accumulators: Vec<LoopAccumulator>,

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
    /// Retry behaviour for the automated step. `None`/absent means the
    /// historical default (`RetryPolicy::default`, 3 immediate retries),
    /// so legacy DSL files keep their prior semantics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<RetryPolicy>,
}

/// DSL payload for an Agent step. Mirrors [`WorkflowNodeData::Agent`]
/// 1:1 — same fields, same defaults — so a graph→DSL→graph round-trip
/// is the identity. PR 1 only models the degenerate (single-turn) path
/// at the compiler; the DSL surface stays full-fidelity so authoring
/// future multi-turn agents needs no DSL schema change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DslAgent {
    pub model: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<serde_json::Value>,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_when: Option<String>,
    #[serde(default)]
    pub context_strategy: ContextStrategy,
    #[serde(default)]
    pub on_tool_error: ToolErrorPolicy,
    #[serde(default)]
    pub retry_policy: RetryPolicy,
    #[serde(default)]
    pub deployment_model: DeploymentModel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DslBranchCondition {
    pub edge: String,
    pub label: String,
    pub guard: String,
}

/// Synthesize a stable edge id from a source/target/handle triple.
/// Mirrors the flow-parser's id scheme so DSL-declared decision branches
/// resolve to the same edges the flow strings create.
pub fn edge_id(source: &str, target: &str, handle: Option<&str>) -> String {
    match handle {
        Some(h) => format!("edge_{}_{}_to_{}", source, h, target),
        None => format!("edge_{}_to_{}", source, target),
    }
}

/// `snake_case` step key → `Title Case` label fallback.
pub fn title_case(s: &str) -> String {
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

/// Extract the target step key from an auto-generated edge ID.
/// e.g. `edge_check_yes_to_process` → `process`.
pub fn extract_edge_target(edge_id: &str) -> String {
    if let Some(pos) = edge_id.rfind("_to_") {
        edge_id[pos + 4..].to_string()
    } else {
        edge_id.to_string()
    }
}

impl WorkflowNodeData {
    /// Build a node payload from a parsed DSL step. The `step_type`
    /// discriminator is data (it comes from YAML/HCL), so this arm is a
    /// string match — but every real variant is handled explicitly and
    /// the fallthrough is an error, never a silently-mistyped node.
    pub fn from_dsl_step(
        key: &str,
        step: &DslStep,
        label: &str,
    ) -> Result<WorkflowNodeData, String> {
        match step.step_type.as_str() {
            "start" => Ok(WorkflowNodeData::Start {
                label: label.to_string(),
                description: step.description.clone(),
                // `initial_data` is the legacy read-compat blob (ignored
                // here). Typed Start ports + process-name now round-trip
                // via the dedicated `initial` / `process_name` fields;
                // absent (legacy files) falls back to the empty-input
                // default so older templates load unchanged.
                initial: step.initial.clone().unwrap_or_else(Port::empty_input),
                process_name: step.process_name.clone(),
            }),
            "end" => Ok(WorkflowNodeData::End {
                label: label.to_string(),
                description: step.description.clone(),
                terminal: default_terminal_port(),
                result_mapping: Vec::new(),
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
                    task_title: step.task_title.clone().unwrap_or_else(|| label.to_string()),
                    instructions_mdsvex: step.instructions.clone(),
                    steps: task_steps,
                    steps_ref: step.steps_ref.clone(),
                    // The legacy text DSL does not model capacity binding;
                    // a DSL-authored human task is always unpooled (byte-
                    // identical to pre-P3). The xyflow JSON path carries
                    // `capacity`/`requirements` via the field derive.
                    capacity: None,
                    requirements: None,
                })
            }
            "agent" => {
                let a = step
                    .agent
                    .as_ref()
                    .ok_or_else(|| format!("agent '{}' requires an 'agent' field", key))?;
                Ok(WorkflowNodeData::Agent {
                    label: label.to_string(),
                    description: step.description.clone(),
                    model: a.model.clone(),
                    system_prompt: a.system_prompt.clone(),
                    user_prompt: a.user_prompt.clone(),
                    response_format: a.response_format.clone(),
                    images: a.images.clone(),
                    max_turns: a.max_turns,
                    stop_when: a.stop_when.clone(),
                    context_strategy: a.context_strategy,
                    on_tool_error: a.on_tool_error,
                    retry_policy: a.retry_policy,
                    deployment_model: a.deployment_model.clone(),
                    // DSL does not model asset bindings (yet).
                    asset_bindings: Vec::new(),
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
                        map.insert(
                            "entrypoint".to_string(),
                            serde_json::Value::String(ep.clone()),
                        );
                    }
                    if !exec.files.is_empty() {
                        let files_arr: Vec<serde_json::Value> = exec
                            .files
                            .iter()
                            .map(|f| serde_json::Value::String(f.clone()))
                            .collect();
                        map.insert(
                            "required_files".to_string(),
                            serde_json::Value::Array(files_arr),
                        );
                    }
                }
                // Parse the backend discriminator via serde — keeps the
                // DSL's accepted value set in lockstep with the wire enum.
                let backend_type: ExecutionBackendType = serde_json::from_value(
                    serde_json::Value::String(exec.backend.clone()),
                )
                .map_err(|_| {
                    format!(
                        "automated_step '{}' has unknown backend '{}' (expected one of: python, process, docker, http, llm, file_ops, kreuzberg, smtp)",
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
                    output: default_output_port(backend_type),
                    // Absent (legacy DSL) → historical default of 3
                    // immediate retries; otherwise round-trip the
                    // authored policy.
                    retry_policy: exec.retry_policy.unwrap_or_default(),
                    // DSL does not model deployment topology — inline.
                    deployment_model: DeploymentModel::default(),
                    // DSL does not model streaming channels (yet).
                    channels: Vec::new(),
                    requirements: None,
                    // DSL does not model asset bindings (yet).
                    asset_bindings: Vec::new(),
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

                let default_branch = step
                    .default_branch
                    .as_ref()
                    .map(|target| edge_id(key, target, None));

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
            "join" => Ok(WorkflowNodeData::Join {
                label: label.to_string(),
                description: step.description.clone(),
                mode: JoinMode::default(),
                merge_strategy: Some(MergeStrategy::default()),
                output: default_join_output_port(),
            }),
            "loop" => {
                let max_iter = step
                    .max_iterations
                    .ok_or_else(|| format!("loop '{}' requires 'max_iterations'", key))?;
                let condition = step
                    .loop_condition
                    .clone()
                    .ok_or_else(|| format!("loop '{}' requires 'loop_condition'", key))?;
                Ok(WorkflowNodeData::Loop {
                    label: label.to_string(),
                    description: step.description.clone(),
                    max_iterations: max_iter,
                    loop_condition: condition,
                    accumulators: step.accumulators.clone(),
                })
            }
            "scope" => Ok(WorkflowNodeData::Scope {
                label: label.to_string(),
                description: step.description.clone(),
            }),
            // The process-control + trigger nodes are GUI-authored: the
            // DSL has no schema for their required fields, and
            // `to_dsl_step` drops them on the way out (documented lossy).
            // They previously fell into the generic catch-all error; keep
            // that behaviour but make it explicit per kind so the
            // round-trip asymmetry is greppable rather than silent.
            "phase_update" | "progress_update" | "failure" | "trigger" | "delay" | "timeout"
            | "map" => Err(format!(
                "step '{}' has GUI-only type '{}' which the DSL format does not model",
                key, step.step_type
            )),
            other => Err(format!("unknown step type '{}' for step '{}'", other, key)),
        }
    }

    /// Project this node payload onto a fresh [`DslStep`]. Exhaustive
    /// `match self` — adding a [`WorkflowNodeData`] variant is a compile
    /// error here until the new variant declares how it serializes (or
    /// explicitly that it's GUI-only and dropped).
    pub fn to_dsl_step(&self, node: &WorkflowNode) -> DslStep {
        let mut step = DslStep {
            step_type: node.node_type.clone(),
            label: Some(self.label().to_string()),
            description: self.description().map(|s| s.to_string()),
            initial_data: None,
            initial: None,
            process_name: None,
            task_title: None,
            instructions: None,
            steps: None,
            steps_ref: None,
            execution: None,
            agent: None,
            conditions: None,
            default_branch: None,
            max_iterations: None,
            loop_condition: None,
            accumulators: Vec::new(),
            children: Vec::new(),
            width: node.width,
            height: node.height,
        };

        match self {
            WorkflowNodeData::Start {
                initial,
                process_name,
                ..
            } => {
                step.initial = Some(initial.clone());
                step.process_name = process_name.clone();
            }
            WorkflowNodeData::End { .. } => {}
            WorkflowNodeData::HumanTask {
                task_title,
                instructions_mdsvex,
                steps: task_steps,
                steps_ref,
                ..
            } => {
                step.task_title = Some(task_title.clone());
                step.instructions = instructions_mdsvex.clone();
                step.steps_ref = steps_ref.clone();
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
                execution_spec,
                retry_policy,
                ..
            } => {
                // Extract entrypoint and files from config into their own
                // fields
                let mut config = execution_spec.config.clone();
                let (entrypoint, files) = if let serde_json::Value::Object(ref mut map) = config {
                    let ep = map
                        .remove("entrypoint")
                        .and_then(|v| v.as_str().map(|s| s.to_string()));
                    let f = map
                        .remove("required_files")
                        .and_then(|v| {
                            v.as_array().map(|arr| {
                                arr.iter()
                                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                        })
                        .unwrap_or_default();
                    (ep, f)
                } else {
                    (None, vec![])
                };
                // Round-trip the enum through serde to recover the
                // canonical snake_case wire string (`python`, `file_ops`,
                // …) so the DSL export matches what users would type.
                let backend = serde_json::to_value(execution_spec.backend_type)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                step.execution = Some(DslExecution {
                    backend,
                    entrypoint,
                    files,
                    config,
                    retry_policy: Some(*retry_policy),
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
                            .map(|bc| DslBranchCondition {
                                edge: extract_edge_target(&bc.edge_id),
                                label: bc.label.clone(),
                                guard: bc.guard.clone(),
                            })
                            .collect(),
                    );
                }
                if let Some(db) = default_branch {
                    step.default_branch = Some(extract_edge_target(db));
                }
            }
            WorkflowNodeData::ParallelSplit { .. } => {}
            WorkflowNodeData::Join { .. } => {
                // Join's mode/merge_strategy/output are GUI-only for now —
                // the DSL has no schema for them. Round-trip through DSL
                // drops the join-specific config, mirroring how
                // process-control nodes behave.
            }
            WorkflowNodeData::Scope { .. } => {
                // children are populated by the CLI envelope after the
                // step map is built
            }
            WorkflowNodeData::LeaseScope { .. } => {
                // children are populated by the CLI envelope after the
                // step map is built; LeaseScope is GUI-authored for now
                // (DSL doesn't model container nodes with lease bindings).
            }
            WorkflowNodeData::Loop {
                max_iterations,
                loop_condition,
                accumulators,
                ..
            } => {
                step.max_iterations = Some(*max_iterations);
                step.loop_condition = Some(loop_condition.clone());
                step.accumulators = accumulators.clone();
            }
            WorkflowNodeData::PhaseUpdate { .. }
            | WorkflowNodeData::ProgressUpdate { .. }
            | WorkflowNodeData::Failure { .. }
            | WorkflowNodeData::Delay { .. }
            | WorkflowNodeData::Timeout { .. }
            | WorkflowNodeData::Map { .. }
            | WorkflowNodeData::StreamSource { .. }
            | WorkflowNodeData::StreamSink { .. } => {
                // DSL doesn't model the process-control / container nodes —
                // GUI-authored for now. Same lossy-drop behaviour as
                // triggers. (Map's body sub-graph + itemsRef/resultVar have
                // no DSL schema yet; stream_source/stream_sink channel
                // declarations likewise have no DSL schema.)
            }
            WorkflowNodeData::Agent {
                model,
                system_prompt,
                user_prompt,
                response_format,
                images,
                max_turns,
                stop_when,
                context_strategy,
                on_tool_error,
                retry_policy,
                deployment_model,
                ..
            } => {
                step.agent = Some(DslAgent {
                    model: model.clone(),
                    system_prompt: system_prompt.clone(),
                    user_prompt: user_prompt.clone(),
                    response_format: response_format.clone(),
                    images: images.clone(),
                    max_turns: *max_turns,
                    stop_when: stop_when.clone(),
                    context_strategy: *context_strategy,
                    on_tool_error: *on_tool_error,
                    retry_policy: *retry_policy,
                    deployment_model: deployment_model.clone(),
                });
            }
            WorkflowNodeData::Trigger { .. } | WorkflowNodeData::SubWorkflow { .. } => {
                // DSL doesn't model triggers or sub-workflows — declared in
                // the GUI for now. Round-trip through DSL drops them,
                // matching how legacy DSL templates behave.
            }
        }

        step
    }
}
