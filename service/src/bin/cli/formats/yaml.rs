use anyhow::Result;

use mekhan_service::models::template::WorkflowGraph;

use super::dsl::DslWorkflow;

pub fn parse(content: &str) -> Result<WorkflowGraph> {
    let dsl: DslWorkflow =
        serde_yaml_ng::from_str(content).map_err(|e| anyhow::anyhow!("invalid YAML: {}", e))?;
    dsl.to_workflow_graph()
        .map_err(|e| anyhow::anyhow!("{}", e))
}

pub fn emit(graph: &WorkflowGraph) -> Result<String> {
    let dsl = DslWorkflow::from_workflow_graph(graph);
    serde_yaml_ng::to_string(&dsl).map_err(|e| anyhow::anyhow!("YAML serialization failed: {}", e))
}
