use anyhow::{Context, Result};
use indexmap::IndexMap;

use mekhan_service::models::template::WorkflowGraph;

use super::dsl::{
    DslBranchCondition, DslExecution, DslStep, DslTaskStep, DslWorkflow,
};

pub fn parse(content: &str) -> Result<WorkflowGraph> {
    let body: hcl::Body = hcl::from_str(content)
        .map_err(|e| anyhow::anyhow!("invalid HCL: {}", e))?;

    let mut steps = IndexMap::new();
    let mut flow_entries: Vec<String> = Vec::new();

    // Top-level attributes are intentionally ignored; only blocks contribute.
    for structure in body.into_iter() {
        if let hcl::Structure::Block(block) = structure {
            let ident = block.identifier().to_string();
            match ident.as_str() {
                "step" => {
                    let key = block
                        .labels()
                        .first()
                        .context("step block requires a label")?
                        .as_str()
                        .to_string();
                    let step = parse_step_block(&block)?;
                    steps.insert(key, step);
                }
                "flow" => {
                    flow_entries = parse_flow_block(&block)?;
                }
                _ => {} // ignore unknown blocks
            }
        }
    }

    let dsl = DslWorkflow {
        steps,
        flow: flow_entries,
    };
    dsl.to_workflow_graph()
        .map_err(|e| anyhow::anyhow!("{}", e))
}

fn parse_step_block(block: &hcl::Block) -> Result<DslStep> {
    let body = block.body();

    let step_type = get_attr_str(body, "type")
        .context("step block requires 'type' attribute")?;

    let mut step = DslStep {
        step_type,
        label: get_attr_str(body, "label"),
        description: get_attr_str(body, "description"),
        initial_data: get_attr_json(body, "initial_data"),
        initial: get_attr_json(body, "initial")
            .and_then(|v| serde_json::from_value(v).ok()),
        process_name: get_attr_str(body, "process_name"),
        task_title: get_attr_str(body, "task_title"),
        instructions: get_attr_str(body, "instructions"),
        steps: None,
        steps_ref: get_attr_str(body, "steps_ref"),
        execution: None,
        agent: None,
        conditions: None,
        default_branch: get_attr_str(body, "default_branch"),
        max_iterations: get_attr_i64(body, "max_iterations").map(|v| v as i32),
        loop_condition: get_attr_str(body, "loop_condition"),
        accumulators: Vec::new(),
        lease: None,
        children: get_attr_string_array(body, "children").unwrap_or_default(),
        width: get_attr_f64(body, "width"),
        height: get_attr_f64(body, "height"),
    };

    // Parse nested blocks
    for structure in body.iter() {
        if let hcl::Structure::Block(inner) = structure {
            match inner.identifier() {
                "execution" => {
                    step.execution = Some(parse_execution_block(inner)?);
                }
                "task_step" => {
                    let ts = parse_task_step_block(inner)?;
                    step.steps.get_or_insert_with(Vec::new).push(ts);
                }
                "condition" => {
                    let cond = parse_condition_block(inner)?;
                    step.conditions.get_or_insert_with(Vec::new).push(cond);
                }
                // `tool_meta {}` blocks were removed when tool naming moved
                // to be derived from the node's own `label` / `description`.
                // Quietly ignore for old HCL files instead of hard-failing
                // (so the rest of the file still parses); the agent compiler
                // takes the label-derived path either way.
                "tool_meta" => {}
                _ => {}
            }
        }
    }

    Ok(step)
}

fn parse_execution_block(block: &hcl::Block) -> Result<DslExecution> {
    let body = block.body();
    let backend = get_attr_str(body, "backend")
        .context("execution block requires 'backend'")?;
    let entrypoint = get_attr_str(body, "entrypoint");
    let files = get_attr_string_array(body, "files").unwrap_or_default();
    let config = get_attr_json(body, "config")
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    let retry_policy = get_attr_json(body, "retry_policy")
        .and_then(|v| serde_json::from_value(v).ok());
    Ok(DslExecution { backend, entrypoint, files, config, retry_policy })
}

fn parse_task_step_block(block: &hcl::Block) -> Result<DslTaskStep> {
    let body = block.body();
    let title = get_attr_str(body, "title")
        .context("task_step block requires 'title'")?;
    let description = get_attr_str(body, "description");

    // Blocks within task_step are complex — store as JSON values
    let mut blocks = Vec::new();
    for structure in body.iter() {
        if let hcl::Structure::Block(inner) = structure {
            if inner.identifier() == "block" {
                if let Ok(json) = hcl_block_to_json(inner) {
                    blocks.push(json);
                }
            }
        }
    }

    Ok(DslTaskStep {
        title,
        description,
        blocks: if blocks.is_empty() { None } else { Some(blocks) },
    })
}

fn parse_condition_block(block: &hcl::Block) -> Result<DslBranchCondition> {
    let body = block.body();
    Ok(DslBranchCondition {
        edge: get_attr_str(body, "edge").context("condition requires 'edge'")?,
        label: get_attr_str(body, "label").context("condition requires 'label'")?,
        guard: get_attr_str(body, "guard").context("condition requires 'guard'")?,
    })
}

fn parse_flow_block(block: &hcl::Block) -> Result<Vec<String>> {
    let body = block.body();
    let mut entries = Vec::new();

    // "chain" attribute: array of step keys → single chain string
    if let Some(chain) = get_attr_string_array(body, "chain") {
        if chain.len() >= 2 {
            entries.push(chain.join(" -> "));
        }
    }

    // "edge" attributes: individual edge strings
    for structure in body.iter() {
        if let hcl::Structure::Attribute(attr) = structure {
            if attr.key() == "edge" {
                if let Some(s) = expr_to_string(attr.expr()) {
                    entries.push(s);
                }
            }
        }
    }

    Ok(entries)
}

// ---------------------------------------------------------------------------
// HCL emit
// ---------------------------------------------------------------------------

pub fn emit(graph: &WorkflowGraph) -> Result<String> {
    let dsl = DslWorkflow::from_workflow_graph(graph);
    let mut lines = Vec::new();

    for (key, step) in &dsl.steps {
        lines.push(format!("step \"{}\" {{", key));
        lines.push(format!("  type = \"{}\"", step.step_type));

        if let Some(ref label) = step.label {
            lines.push(format!("  label = \"{}\"", escape_hcl_str(label)));
        }
        if let Some(ref desc) = step.description {
            lines.push(format!("  description = \"{}\"", escape_hcl_str(desc)));
        }
        if let Some(ref data) = step.initial_data {
            lines.push(format!(
                "  initial_data = {}",
                serde_json::to_string(data).unwrap_or_default()
            ));
        }
        if let Some(ref initial) = step.initial {
            if let Ok(v) = serde_json::to_value(initial) {
                lines.push(format!(
                    "  initial = {}",
                    serde_json::to_string(&v).unwrap_or_default()
                ));
            }
        }
        if let Some(ref pn) = step.process_name {
            lines.push(format!("  process_name = \"{}\"", escape_hcl_str(pn)));
        }
        if let Some(ref tt) = step.task_title {
            lines.push(format!("  task_title = \"{}\"", escape_hcl_str(tt)));
        }
        if let Some(ref inst) = step.instructions {
            lines.push(format!("  instructions = <<-EOT\n{}\n  EOT", inst.trim_end()));
        }
        if let Some(ref exec) = step.execution {
            lines.push(String::new());
            lines.push("  execution {".to_string());
            lines.push(format!("    backend = \"{}\"", exec.backend));
            if let Some(ref ep) = exec.entrypoint {
                lines.push(format!("    entrypoint = \"{}\"", escape_hcl_str(ep)));
            }
            if !exec.files.is_empty() {
                let quoted: Vec<String> = exec.files.iter().map(|f| format!("\"{}\"", escape_hcl_str(f))).collect();
                lines.push(format!("    files = [{}]", quoted.join(", ")));
            }
            lines.push(format!(
                "    config = {}",
                serde_json::to_string_pretty(&exec.config)
                    .unwrap_or_default()
                    .replace('\n', "\n    ")
            ));
            if let Some(ref rp) = exec.retry_policy {
                if let Ok(v) = serde_json::to_value(rp) {
                    lines.push(format!(
                        "    retry_policy = {}",
                        serde_json::to_string(&v).unwrap_or_default()
                    ));
                }
            }
            lines.push("  }".to_string());
        }
        if let Some(ref conditions) = step.conditions {
            for cond in conditions {
                lines.push(String::new());
                lines.push("  condition {".to_string());
                lines.push(format!("    edge = \"{}\"", cond.edge));
                lines.push(format!("    label = \"{}\"", escape_hcl_str(&cond.label)));
                lines.push(format!("    guard = \"{}\"", escape_hcl_str(&cond.guard)));
                lines.push("  }".to_string());
            }
        }
        if let Some(ref db) = step.default_branch {
            lines.push(format!("  default_branch = \"{}\"", db));
        }
        if let Some(mi) = step.max_iterations {
            lines.push(format!("  max_iterations = {}", mi));
        }
        if let Some(ref lc) = step.loop_condition {
            lines.push(format!("  loop_condition = \"{}\"", escape_hcl_str(lc)));
        }
        if !step.children.is_empty() {
            let quoted: Vec<String> = step.children.iter().map(|c| format!("\"{}\"", escape_hcl_str(c))).collect();
            lines.push(format!("  children = [{}]", quoted.join(", ")));
        }
        if let Some(w) = step.width {
            lines.push(format!("  width = {}", w));
        }
        if let Some(h) = step.height {
            lines.push(format!("  height = {}", h));
        }
        if let Some(ref task_steps) = step.steps {
            for ts in task_steps {
                lines.push(String::new());
                lines.push(format!("  task_step \"{}\" {{", escape_hcl_str(&ts.title)));
                lines.push(format!("    title = \"{}\"", escape_hcl_str(&ts.title)));
                if let Some(ref desc) = ts.description {
                    lines.push(format!("    description = \"{}\"", escape_hcl_str(desc)));
                }
                if let Some(ref blocks) = ts.blocks {
                    for block_val in blocks {
                        emit_task_block(&mut lines, block_val, 4);
                    }
                }
                lines.push("  }".to_string());
            }
        }

        lines.push("}".to_string());
        lines.push(String::new());
    }

    // Flow block
    if !dsl.flow.is_empty() {
        lines.push("flow {".to_string());
        for entry in &dsl.flow {
            // If it's a simple chain (no brackets), use chain attribute
            if !entry.contains('[') && entry.matches("->").count() >= 1 {
                let steps: Vec<&str> = entry.split("->").map(str::trim).collect();
                let quoted: Vec<String> = steps.iter().map(|s| format!("\"{}\"", s)).collect();
                lines.push(format!("  chain = [{}]", quoted.join(", ")));
            } else {
                lines.push(format!("  edge = \"{}\"", entry));
            }
        }
        lines.push("}".to_string());
        lines.push(String::new());
    }

    Ok(lines.join("\n"))
}

// ---------------------------------------------------------------------------
// HCL helpers
// ---------------------------------------------------------------------------

fn get_attr_str(body: &hcl::Body, key: &str) -> Option<String> {
    body.iter()
        .find_map(|s| match s {
            hcl::Structure::Attribute(attr) if attr.key() == key => {
                expr_to_string(attr.expr())
            }
            _ => None,
        })
}

fn get_attr_f64(body: &hcl::Body, key: &str) -> Option<f64> {
    body.iter()
        .find_map(|s| match s {
            hcl::Structure::Attribute(attr) if attr.key() == key => {
                match attr.expr() {
                    hcl::Expression::Number(n) => n.as_f64(),
                    _ => None,
                }
            }
            _ => None,
        })
}

fn get_attr_i64(body: &hcl::Body, key: &str) -> Option<i64> {
    body.iter()
        .find_map(|s| match s {
            hcl::Structure::Attribute(attr) if attr.key() == key => {
                expr_to_i64(attr.expr())
            }
            _ => None,
        })
}

fn get_attr_json(body: &hcl::Body, key: &str) -> Option<serde_json::Value> {
    body.iter()
        .find_map(|s| match s {
            hcl::Structure::Attribute(attr) if attr.key() == key => {
                expr_to_json(attr.expr())
            }
            _ => None,
        })
}

fn get_attr_string_array(body: &hcl::Body, key: &str) -> Option<Vec<String>> {
    body.iter()
        .find_map(|s| match s {
            hcl::Structure::Attribute(attr) if attr.key() == key => {
                if let hcl::Expression::Array(arr) = attr.expr() {
                    let strings: Vec<String> = arr
                        .iter()
                        .filter_map(expr_to_string)
                        .collect();
                    if strings.is_empty() {
                        None
                    } else {
                        Some(strings)
                    }
                } else {
                    None
                }
            }
            _ => None,
        })
}

fn expr_to_string(expr: &hcl::Expression) -> Option<String> {
    match expr {
        hcl::Expression::String(s) => Some(s.to_string()),
        _ => {
            // Try to extract string from other expression types (heredocs, templates)
            let s = expr.to_string();
            if s.starts_with('"') && s.ends_with('"') {
                Some(s[1..s.len() - 1].to_string())
            } else if !s.is_empty() {
                Some(s)
            } else {
                None
            }
        }
    }
}

fn expr_to_i64(expr: &hcl::Expression) -> Option<i64> {
    match expr {
        hcl::Expression::Number(n) => {
            // Convert hcl::Number to i64
            if let Some(i) = n.as_i64() {
                Some(i)
            } else {
                n.as_f64().map(|f| f as i64)
            }
        }
        _ => None,
    }
}

fn expr_to_json(expr: &hcl::Expression) -> Option<serde_json::Value> {
    match expr {
        hcl::Expression::String(s) => Some(serde_json::Value::String(s.to_string())),
        hcl::Expression::Number(n) => {
            // Convert hcl::Number → serde_json::Number
            if let Some(i) = n.as_i64() {
                Some(serde_json::json!(i))
            } else {
                n.as_f64().map(|f| serde_json::json!(f))
            }
        }
        hcl::Expression::Bool(b) => Some(serde_json::Value::Bool(*b)),
        hcl::Expression::Object(obj) => {
            let mut map = serde_json::Map::new();
            for (k, v) in obj {
                let key = match k {
                    hcl::ObjectKey::Identifier(id) => id.to_string(),
                    hcl::ObjectKey::Expression(e) => {
                        expr_to_string(e).unwrap_or_default()
                    }
                    _ => continue,
                };
                if let Some(val) = expr_to_json(v) {
                    map.insert(key, val);
                }
            }
            Some(serde_json::Value::Object(map))
        }
        hcl::Expression::Array(arr) => {
            let items: Vec<serde_json::Value> = arr
                .iter()
                .filter_map(expr_to_json)
                .collect();
            Some(serde_json::Value::Array(items))
        }
        _ => None,
    }
}

fn hcl_block_to_json(block: &hcl::Block) -> Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    let block_type = block
        .labels()
        .first()
        .map(|l| l.as_str().to_string())
        .unwrap_or_else(|| block.identifier().to_string());
    map.insert("type".to_string(), serde_json::Value::String(block_type));

    for structure in block.body().iter() {
        if let hcl::Structure::Attribute(attr) = structure {
            if let Some(val) = expr_to_json(attr.expr()) {
                map.insert(attr.key().to_string(), val);
            }
        }
    }

    Ok(serde_json::Value::Object(map))
}

/// Emit a task block (form field) as an HCL `block "type" { ... }` structure.
fn emit_task_block(lines: &mut Vec<String>, val: &serde_json::Value, indent: usize) {
    let pad = " ".repeat(indent);
    let inner_pad = " ".repeat(indent + 2);

    let obj = match val.as_object() {
        Some(o) => o,
        None => return,
    };

    let block_type = obj.get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    lines.push(String::new());
    lines.push(format!("{pad}block \"{block_type}\" {{"));

    for (key, value) in obj {
        if key == "type" {
            continue;
        }
        lines.push(format!(
            "{inner_pad}{} = {}",
            key,
            json_to_hcl_value(value)
        ));
    }

    lines.push(format!("{pad}}}"));
}

/// Convert a serde_json::Value to an inline HCL value string.
fn json_to_hcl_value(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(s) => format!("\"{}\"", escape_hcl_str(s)),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_to_hcl_value).collect();
            format!("[{}]", items.join(", "))
        }
        serde_json::Value::Object(obj) => {
            let entries: Vec<String> = obj.iter()
                .map(|(k, v)| format!("{} = {}", k, json_to_hcl_value(v)))
                .collect();
            format!("{{ {} }}", entries.join(", "))
        }
    }
}

fn escape_hcl_str(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
