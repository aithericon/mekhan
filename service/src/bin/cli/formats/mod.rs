pub mod dsl;
pub mod hcl;
pub mod layout;
pub mod yaml;

use std::fmt;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use mekhan_service::models::template::WorkflowGraph;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowFormat {
    Json,
    Yaml,
    Hcl,
}

impl WorkflowFormat {
    pub fn filename(&self) -> &'static str {
        match self {
            Self::Json => "graph.json",
            Self::Yaml => "workflow.yaml",
            Self::Hcl => "workflow.hcl",
        }
    }

    pub fn from_str_arg(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "json" => Ok(Self::Json),
            "yaml" | "yml" => Ok(Self::Yaml),
            "hcl" => Ok(Self::Hcl),
            _ => bail!("unknown format '{}' — expected json, yaml, or hcl", s),
        }
    }
}

impl fmt::Display for WorkflowFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json => write!(f, "json"),
            Self::Yaml => write!(f, "yaml"),
            Self::Hcl => write!(f, "hcl"),
        }
    }
}

/// Detect which format file exists in a directory.
pub fn detect_format(dir: &Path) -> Result<WorkflowFormat> {
    if dir.join("workflow.yaml").exists() {
        Ok(WorkflowFormat::Yaml)
    } else if dir.join("workflow.hcl").exists() {
        Ok(WorkflowFormat::Hcl)
    } else if dir.join("graph.json").exists() {
        Ok(WorkflowFormat::Json)
    } else {
        bail!("no workflow definition found (expected workflow.yaml, workflow.hcl, or graph.json)")
    }
}

/// Read a workflow file in the detected/specified format.
pub fn read_workflow(dir: &Path, format: WorkflowFormat) -> Result<WorkflowGraph> {
    let path = dir.join(format.filename());
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    match format {
        WorkflowFormat::Json => serde_json::from_str(&content).context("invalid graph.json"),
        WorkflowFormat::Yaml => yaml::parse(&content),
        WorkflowFormat::Hcl => hcl::parse(&content),
    }
}

/// Write a workflow file in the specified format.
pub fn write_workflow(dir: &Path, format: WorkflowFormat, graph: &WorkflowGraph) -> Result<()> {
    let content = match format {
        WorkflowFormat::Json => serde_json::to_string_pretty(graph)?,
        WorkflowFormat::Yaml => yaml::emit(graph)?,
        WorkflowFormat::Hcl => hcl::emit(graph)?,
    };

    let path = dir.join(format.filename());
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}
