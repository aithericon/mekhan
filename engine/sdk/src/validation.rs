//! Pre-flight validation for scenarios.
//!
//! Validates:
//! - Rhai script syntax
//! - Rhai variable bindings (variables must match port names)
//! - Orphan places (no arcs)
//! - Port connectivity
//!
//! # Example
//! ```ignore
//! let scenario = ctx.build();
//! let result = validate(&scenario);
//!
//! if !result.is_valid {
//!     for err in &result.errors {
//!         eprintln!("Error: {}", err);
//!     }
//!     std::process::exit(1);
//! }
//! ```

use regex::Regex;
use rhai::Engine;
use std::collections::HashSet;

use crate::scenario::{ScenarioDefinition, TransitionGuard, TransitionLogic};

/// Validation result
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// True if no errors (warnings allowed)
    pub is_valid: bool,
    /// Critical errors that would cause runtime failure
    pub errors: Vec<String>,
    /// Warnings about potential issues
    pub warnings: Vec<String>,
}

impl ValidationResult {
    fn new() -> Self {
        Self {
            is_valid: true,
            errors: vec![],
            warnings: vec![],
        }
    }

    fn add_error(&mut self, msg: impl Into<String>) {
        self.errors.push(msg.into());
        self.is_valid = false;
    }

    fn add_warning(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }
}

/// Validate Rhai script syntax
pub fn validate_script(script: &str) -> Result<(), String> {
    let engine = Engine::new();
    engine
        .compile(script)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Rhai keywords and built-ins to exclude from variable extraction
const RHAI_KEYWORDS: &[&str] = &[
    "true",
    "false",
    "let",
    "const",
    "if",
    "else",
    "for",
    "while",
    "loop",
    "fn",
    "this",
    "return",
    "throw",
    "try",
    "catch",
    "in",
    "is",
    "break",
    "continue",
    "switch",
    "do",
    "until",
    "import",
    "export",
    "as",
    "private",
    "print",
    "debug",
    "type_of",
    "call",
    "curry",
    "len",
    "keys",
    "values",
    "contains",
    "get",
    "set",
    "take",
    "drain",
    "retain",
    "splice",
    "push",
    "pop",
    "shift",
    "insert",
    "remove",
    "append",
    "pad",
    "clear",
    "truncate",
    "reverse",
    "sort",
    "filter",
    "map",
    "reduce",
    "find",
    "any",
    "all",
    "some",
    "none",
    "min",
    "max",
    "sum",
    "product",
    "to_string",
    "to_int",
    "to_float",
    "to_bool",
    "to_array",
    "to_blob",
    "parse_int",
    "parse_float",
    "split",
    "trim",
    "to_upper",
    "to_lower",
    "starts_with",
    "ends_with",
    "index_of",
    "sub_string",
    "replace",
    "abs",
    "floor",
    "ceiling",
    "round",
    "sqrt",
    "sin",
    "cos",
    "tan",
    "log",
    "exp",
    "is_nan",
    "is_finite",
    "is_infinite",
    "asin",
    "acos",
    "atan",
    "rand",
    "timestamp",
    "elapsed",
];

/// Extract top-level variable references from a Rhai script.
/// Finds variables at the start of property access chains (e.g., `job.id` -> `job`).
/// Excludes local variables defined with `let`.
fn extract_script_variables(script: &str) -> HashSet<String> {
    let mut vars = HashSet::new();
    let mut local_vars = HashSet::new();

    // Strip comments and strings before analysis
    let script = strip_rhai_comments(script);

    // First pass: find all local variable definitions (let x = ...) and for-loop variables (for x in ...)
    let re_let = Regex::new(r"\blet\s+([a-z_][a-z0-9_]*)").unwrap();
    for cap in re_let.captures_iter(&script) {
        if let Some(var) = cap.get(1) {
            local_vars.insert(var.as_str().to_string());
        }
    }
    let re_for = Regex::new(r"\bfor\s+([a-z_][a-z0-9_]*)\s+in\b").unwrap();
    for cap in re_for.captures_iter(&script) {
        if let Some(var) = cap.get(1) {
            local_vars.insert(var.as_str().to_string());
        }
    }

    // Match variable.property access patterns (e.g., job.id, worker.name)
    let re_property = Regex::new(r"\b([a-z_][a-z0-9_]*)\s*\.").unwrap();
    for mat in re_property.find_iter(&script) {
        // Check if this match is preceded by a dot (chained property access)
        // If so, skip it - it's not a top-level variable
        let start = mat.start();
        if start > 0 {
            let prev_char = script.chars().nth(start - 1).unwrap_or(' ');
            if prev_char == '.' {
                continue; // Skip chained property access like req.patient_id.len()
            }
        }

        // Extract the variable name from the match
        if let Some(cap) = re_property.captures(mat.as_str()) {
            let var = cap.get(1).unwrap().as_str();
            // Skip keywords, local variables, and common Rhai builtins
            if !RHAI_KEYWORDS.contains(&var) && !local_vars.contains(var) {
                vars.insert(var.to_string());
            }
        }
    }

    vars
}

/// Strip Rhai comments and string literals from script to avoid false positives.
/// String literals are replaced with empty quotes to preserve structure.
fn strip_rhai_comments(script: &str) -> String {
    let mut result = String::new();
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_double_string = false;
    let mut in_single_string = false;
    let mut prev_char = ' ';
    let mut chars = script.chars().peekable();

    while let Some(c) = chars.next() {
        // Handle escape sequences in strings
        if (in_double_string || in_single_string) && prev_char == '\\' {
            // Skip escaped character, don't update prev_char to avoid \\\" issues
            prev_char = ' ';
            continue;
        }

        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
                result.push(c);
            }
        } else if in_block_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
            }
        } else if in_double_string {
            if c == '"' {
                in_double_string = false;
                result.push('"'); // Close the empty string
            }
            // Skip string content
        } else if in_single_string {
            if c == '\'' {
                in_single_string = false;
                result.push('\''); // Close the empty string
            }
            // Skip string content
        } else if c == '/' {
            match chars.peek() {
                Some('/') => {
                    chars.next();
                    in_line_comment = true;
                }
                Some('*') => {
                    chars.next();
                    in_block_comment = true;
                }
                _ => result.push(c),
            }
        } else if c == '"' {
            in_double_string = true;
            result.push('"'); // Start empty string placeholder
        } else if c == '\'' {
            in_single_string = true;
            result.push('\''); // Start empty string placeholder
        } else {
            result.push(c);
        }

        prev_char = c;
    }

    result
}

/// Validate that script variables match available input ports.
/// Only checks that variables used with `.property` access exist as input ports.
fn validate_script_variables(
    script: &str,
    input_ports: &[String],
    result: &mut ValidationResult,
    transition_id: &str,
    context: &str,
) {
    let script_vars = extract_script_variables(script);
    let available: HashSet<_> = input_ports.iter().cloned().collect();

    for var in &script_vars {
        if !available.contains(var) {
            result.add_error(format!(
                "Transition '{}' {}: script references undefined variable '{}'. Available input ports: {:?}",
                transition_id, context, var, input_ports
            ));
        }
    }
}

/// Validate a Rhai script immediately during transition building.
///
/// Returns a list of error messages. Empty list means validation passed.
/// This is called directly by TransitionBuilder::logic() for fail-fast validation.
pub fn validate_script_inline(
    script: &str,
    input_port_names: &[String],
    transition_id: &str,
) -> Vec<String> {
    let mut errors = Vec::new();

    // 1. Syntax validation
    if let Err(e) = validate_script(script) {
        errors.push(format!(
            "Transition '{}': Rhai syntax error: {}",
            transition_id, e
        ));
        // Don't continue with variable validation if syntax is broken
        return errors;
    }

    // 2. Variable binding validation
    let script_vars = extract_script_variables(script);
    let available: HashSet<_> = input_port_names.iter().cloned().collect();

    for var in &script_vars {
        if !available.contains(var) {
            errors.push(format!(
                "Transition '{}': script references undefined variable '{}'. Available input ports: {:?}",
                transition_id, var, input_port_names
            ));
        }
    }

    errors
}

/// Check for orphan places (no arcs connected)
pub fn check_orphans(scenario: &ScenarioDefinition) -> Vec<String> {
    let mut connected_places: HashSet<&str> = HashSet::new();

    // Collect all places referenced by arcs
    for transition in &scenario.transitions {
        for arc in &transition.inputs {
            connected_places.insert(&arc.place);
        }
        for arc in &transition.outputs {
            connected_places.insert(&arc.place);
        }
    }

    // Find places with no connections
    scenario
        .places
        .iter()
        .filter(|p| !connected_places.contains(p.id.as_str()))
        .map(|p| format!("Place '{}' ({}) has no connections", p.id, p.name))
        .collect()
}

/// Check for disconnected ports
pub fn check_port_connectivity(scenario: &ScenarioDefinition) -> Vec<String> {
    let mut warnings = vec![];

    for transition in &scenario.transitions {
        let input_arc_ports: HashSet<_> =
            transition.inputs.iter().map(|a| a.port.as_str()).collect();
        let output_arc_ports: HashSet<_> =
            transition.outputs.iter().map(|a| a.port.as_str()).collect();

        // Check input ports are wired
        for port in &transition.input_ports {
            if !input_arc_ports.contains(port.name.as_str()) {
                warnings.push(format!(
                    "Transition '{}': input port '{}' has no incoming arc",
                    transition.id, port.name
                ));
            }
        }

        // Check output ports are wired
        for port in &transition.output_ports {
            if !output_arc_ports.contains(port.name.as_str()) {
                warnings.push(format!(
                    "Transition '{}': output port '{}' has no outgoing arc - data will be discarded",
                    transition.id, port.name
                ));
            }
        }
    }

    warnings
}

/// Full validation of a scenario
pub fn validate(scenario: &ScenarioDefinition) -> ValidationResult {
    let mut result = ValidationResult::new();

    // Validate all Rhai scripts
    for transition in &scenario.transitions {
        // Collect input port names for variable validation
        let input_names: Vec<_> = transition
            .input_ports
            .iter()
            .map(|p| p.name.clone())
            .collect();

        // Check main logic
        if let TransitionLogic::Rhai { source } = &transition.logic {
            // Syntax validation
            if let Err(e) = validate_script(source) {
                result.add_error(format!(
                    "Transition '{}' logic script error: {}",
                    transition.id, e
                ));
            }

            // Variable binding validation
            validate_script_variables(source, &input_names, &mut result, &transition.id, "logic");
        }

        // Check guard
        if let Some(TransitionGuard::Rhai { source }) = &transition.guard {
            // Syntax validation
            if let Err(e) = validate_script(source) {
                result.add_error(format!(
                    "Transition '{}' guard script error: {}",
                    transition.id, e
                ));
            }

            // Variable binding validation (guards only use input ports)
            validate_script_variables(source, &input_names, &mut result, &transition.id, "guard");
        }
    }

    // Check for orphan places
    for warning in check_orphans(scenario) {
        result.add_warning(warning);
    }

    // Check port connectivity
    for warning in check_port_connectivity(scenario) {
        result.add_warning(warning);
    }

    result
}

// ---------------------------------------------------------------------------
// Mock-based Rhai validation
// ---------------------------------------------------------------------------

/// Generate mock JSON data from a JSON Schema definition.
///
/// Walks the schema tree and produces deterministic mock values suitable for
/// validating Rhai scripts at test time. Not intended for fuzzing — just
/// catches field-name typos, reserved keywords, and type mismatches.
pub fn mock_from_schema(
    schema: &serde_json::Value,
    definitions: &std::collections::HashMap<String, serde_json::Value>,
) -> serde_json::Value {
    // Handle $ref
    if let Some(ref_str) = schema.get("$ref").and_then(|v| v.as_str()) {
        if let Some(type_name) = ref_str.strip_prefix("#/definitions/") {
            if let Some(def) = definitions.get(type_name) {
                return mock_from_schema(def, definitions);
            }
        }
        return serde_json::Value::Null;
    }

    match schema.get("type").and_then(|v| v.as_str()) {
        Some("string") => serde_json::Value::String("mock_string".into()),
        Some("number") => serde_json::json!(1.0),
        Some("integer") => serde_json::json!(1),
        Some("boolean") => serde_json::json!(true),
        Some("object") => {
            let mut map = serde_json::Map::new();
            if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
                for (key, prop_schema) in props {
                    map.insert(key.clone(), mock_from_schema(prop_schema, definitions));
                }
            }
            if map.is_empty() {
                // Opaque object (e.g., serde_json::Value) — provide a non-empty stub
                map.insert("_mock".into(), serde_json::json!("mock_value"));
            }
            serde_json::Value::Object(map)
        }
        Some("array") => {
            let item = schema
                .get("items")
                .map(|items| mock_from_schema(items, definitions))
                .unwrap_or(serde_json::Value::Null);
            serde_json::json!([item])
        }
        _ => {
            // Handle anyOf/oneOf (pick first variant)
            if let Some(any_of) = schema.get("anyOf").or(schema.get("oneOf")) {
                if let Some(first) = any_of.as_array().and_then(|a| a.first()) {
                    return mock_from_schema(first, definitions);
                }
            }
            // Fallback: true schema (accepts anything) or unknown
            serde_json::Value::Null
        }
    }
}

/// Convert a `serde_json::Value` into a Rhai `Dynamic`.
///
/// Mirrors the logic in `petri-application::rhai_runtime::json_to_dynamic`.
fn json_to_dynamic(value: &serde_json::Value) -> rhai::Dynamic {
    match value {
        serde_json::Value::Null => rhai::Dynamic::UNIT,
        serde_json::Value::Bool(b) => rhai::Dynamic::from(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                rhai::Dynamic::from(i)
            } else if let Some(f) = n.as_f64() {
                rhai::Dynamic::from(f)
            } else {
                rhai::Dynamic::UNIT
            }
        }
        serde_json::Value::String(s) => rhai::Dynamic::from(s.clone()),
        serde_json::Value::Array(arr) => {
            let items: Vec<rhai::Dynamic> = arr.iter().map(json_to_dynamic).collect();
            rhai::Dynamic::from(items)
        }
        serde_json::Value::Object(map) => {
            let mut rhai_map = rhai::Map::new();
            for (k, v) in map {
                rhai_map.insert(k.clone().into(), json_to_dynamic(v));
            }
            rhai::Dynamic::from(rhai_map)
        }
    }
}

/// Validate all Rhai transition scripts by executing them with mock data.
///
/// For each transition with Rhai logic:
/// 1. Generates mock JSON data from input port schemas
/// 2. Executes the script with the mock inputs
/// 3. Reports any runtime errors (reserved keywords, field access failures, etc.)
///
/// Does NOT validate output correctness — only that scripts execute without errors.
///
/// # Example
/// ```ignore
/// let mut ctx = Context::new("test");
/// definition(&mut ctx);
/// let scenario = ctx.build();
/// let result = validate_with_mocks(&scenario);
/// assert!(result.is_valid, "{:?}", result.errors);
/// ```
pub fn validate_with_mocks(scenario: &ScenarioDefinition) -> ValidationResult {
    let mut result = ValidationResult::new();
    let engine = Engine::new();

    for transition in &scenario.transitions {
        let source = match &transition.logic {
            TransitionLogic::Rhai { source } => source,
            _ => continue,
        };

        // Build mock scope from input port schemas
        let mut scope = rhai::Scope::new();
        for port in &transition.input_ports {
            let mock = if let Some(ref schema_ref) = port.schema_ref {
                let type_name = schema_ref
                    .strip_prefix("#/definitions/")
                    .unwrap_or(schema_ref);
                if let Some(schema) = scenario.definitions.get(type_name) {
                    let mock_json = mock_from_schema(schema, &scenario.definitions);
                    // Wrap batch ports in an array
                    if port.cardinality == "batch" {
                        json_to_dynamic(&serde_json::json!([mock_json]))
                    } else {
                        json_to_dynamic(&mock_json)
                    }
                } else {
                    rhai::Dynamic::UNIT
                }
            } else {
                rhai::Dynamic::UNIT
            };
            scope.push_dynamic(&port.name, mock);
        }

        match engine.eval_with_scope::<rhai::Dynamic>(&mut scope, source) {
            Ok(_) => {} // Script executed successfully
            Err(e) => {
                result.add_error(format!(
                    "Transition '{}' ({}): mock execution failed: {}",
                    transition.id,
                    transition.name,
                    e
                ));
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_script() {
        assert!(validate_script("let x = 1; x + 1").is_ok());
        assert!(validate_script(r#"#{ foo: 42 }"#).is_ok());
    }

    #[test]
    fn test_invalid_script() {
        assert!(validate_script("let x = ;").is_err());
        assert!(validate_script("if { }").is_err());
    }

    #[test]
    fn test_extract_script_variables_simple() {
        let script = r#"#{ result: #{ task_id: task.id, worker_id: worker.id } }"#;
        let vars = extract_script_variables(script);
        assert!(vars.contains("task"));
        assert!(vars.contains("worker"));
        assert_eq!(vars.len(), 2);
    }

    #[test]
    fn test_extract_script_variables_with_comments() {
        let script = r#"
            // This is task.fake - should be ignored
            #{ result: job.id }
        "#;
        let vars = extract_script_variables(script);
        assert!(vars.contains("job"));
        assert!(!vars.contains("task")); // Comment should be stripped
        assert_eq!(vars.len(), 1);
    }

    #[test]
    fn test_extract_script_variables_filters_keywords() {
        // "print" is a Rhai built-in and should be filtered
        let script = r#"print.something; job.id"#;
        let vars = extract_script_variables(script);
        assert!(vars.contains("job"));
        assert!(!vars.contains("print"));
    }

    #[test]
    fn test_strip_rhai_comments() {
        let script = "// comment\ncode // inline\n/* block */ more";
        let stripped = strip_rhai_comments(script);
        assert!(!stripped.contains("comment"));
        assert!(!stripped.contains("inline"));
        assert!(!stripped.contains("block"));
        assert!(stripped.contains("code"));
        assert!(stripped.contains("more"));
    }

    // ── Mock schema tests ────────────────────────────────────────────────

    #[test]
    fn test_mock_from_schema_primitives() {
        let defs = std::collections::HashMap::new();
        assert_eq!(mock_from_schema(&serde_json::json!({"type": "string"}), &defs), serde_json::json!("mock_string"));
        assert_eq!(mock_from_schema(&serde_json::json!({"type": "integer"}), &defs), serde_json::json!(1));
        assert_eq!(mock_from_schema(&serde_json::json!({"type": "number"}), &defs), serde_json::json!(1.0));
        assert_eq!(mock_from_schema(&serde_json::json!({"type": "boolean"}), &defs), serde_json::json!(true));
    }

    #[test]
    fn test_mock_from_schema_object() {
        let defs = std::collections::HashMap::new();
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "count": { "type": "integer" }
            }
        });
        let mock = mock_from_schema(&schema, &defs);
        assert_eq!(mock["name"], serde_json::json!("mock_string"));
        assert_eq!(mock["count"], serde_json::json!(1));
    }

    #[test]
    fn test_mock_from_schema_ref() {
        let mut defs = std::collections::HashMap::new();
        defs.insert("MyType".into(), serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "string" } }
        }));
        let schema = serde_json::json!({"$ref": "#/definitions/MyType"});
        let mock = mock_from_schema(&schema, &defs);
        assert_eq!(mock["id"], serde_json::json!("mock_string"));
    }

    #[test]
    fn test_mock_from_schema_array() {
        let defs = std::collections::HashMap::new();
        let schema = serde_json::json!({
            "type": "array",
            "items": { "type": "string" }
        });
        let mock = mock_from_schema(&schema, &defs);
        assert_eq!(mock, serde_json::json!(["mock_string"]));
    }

    #[test]
    fn test_validate_with_mocks_catches_missing_field() {
        use crate::context::Context;
        use schemars::JsonSchema;
        use serde::Serialize;

        #[derive(Clone, Debug, Serialize, JsonSchema)]
        struct TestInput {
            id: String,
            value: f64,
        }

        let mut ctx = Context::new("test-mock-validation");
        let input_place = ctx.state::<TestInput>("input", "Input");
        let output_place = ctx.state::<TestInput>("output", "Output");

        // This script accesses `data.nonexistent_field` — syntax is valid
        // but mock execution will fail because the field doesn't exist
        ctx.transition("bad_transition", "Bad Transition")
            .auto_input("data", &input_place)
            .auto_output("out", &output_place)
            .logic(r#"
                let x = data.nested.deep.field;
                #{ out: #{ id: data.id, value: x } }
            "#);

        let scenario = ctx.build();
        let result = validate_with_mocks(&scenario);

        // Should report an error — `data.nested` doesn't exist on TestInput
        assert!(!result.is_valid, "Expected mock validation to catch missing nested field");
        assert!(
            result.errors.iter().any(|e| e.contains("bad_transition")),
            "Error should reference the transition: {:?}", result.errors
        );
    }

    #[test]
    fn test_validate_with_mocks_passes_valid_script() {
        use crate::context::Context;
        use schemars::JsonSchema;
        use serde::Serialize;

        #[derive(Clone, Debug, Serialize, JsonSchema)]
        struct Item {
            name: String,
            count: i64,
        }

        let mut ctx = Context::new("test-mock-valid");
        let items = ctx.state::<Item>("items", "Items");
        let results = ctx.state::<Item>("results", "Results");

        ctx.transition("process", "Process Item")
            .auto_input("item", &items)
            .auto_output("result", &results)
            .logic(r#"#{ result: #{ name: item.name, count: item.count + 1 } }"#);

        let scenario = ctx.build();
        let result = validate_with_mocks(&scenario);

        assert!(result.is_valid, "Valid script should pass: {:?}", result.errors);
    }
}
