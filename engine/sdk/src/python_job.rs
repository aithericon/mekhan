//! Typed Python job dispatch helpers.
//!
//! Eliminates the 30+ line Rhai boilerplate for constructing `SchedulerSubmitInput`
//! tokens when dispatching Python executor jobs.
//!
//! ## Two levels of API
//!
//! **Level 1 — Rhai generation** via [`python_job_rhai`]: Returns a Rhai map literal
//! string. Use when you need custom transition wiring (e.g., extra `read_input_batch`).
//! For batch inputs from `read_input_batch`, use [`python_job_rhai_with_dynamic`].
//!
//! **Level 2 — Full wiring** via [`PythonJobDispatch::wire`]: Creates the dispatch
//! transition + pending place in one call. For the common trigger→dispatch→pending pattern.
//! When `script_content` is set on the config, `wire()` auto-registers the script as
//! a Rhai variable and prepends a raw input — no manual `ctx.rhai_var()` needed.
//!
//! ## Example
//!
//! ```ignore
//! use aithericon_sdk::prelude::*;
//! use aithericon_sdk::python_job::*;
//!
//! let config = PythonJobConfig {
//!     script_content: include_str!("../python/my_script.py").into(),
//!     script_filename: "my_script.py".into(),
//!     requirements: vec!["numpy".into(), "scipy".into()],
//!     max_retries: 1,
//!     virtualenv: true,
//!     sdk: true,
//!     python: None,
//!     stream_events: None,
//!     nix_packages: None,
//! };
//!
//! // Level 1: just generate the Rhai (caller manages rhai_var + script input)
//! let var = script_var_name(&config.script_filename);
//! ctx.rhai_var(&var, &config.script_content);
//! let rhai = python_job_rhai(
//!     r#"trigger.id + ":run""#,
//!     "my-job",
//!     &config,
//!     &[
//!         JobInput::script("my_script.py", &var),
//!         JobInput::inline("data", "trigger.data"),
//!     ],
//!     &[JobOutput::new("result")],
//! );
//!
//! // Level 2: full wiring (script_content auto-registered + auto-added as input)
//! let pending = PythonJobDispatch {
//!     id: "dispatch_job",
//!     label: "Dispatch Job",
//!     trigger: &trigger_place,
//!     job_queue: &to_jobs,
//!     job_id_expr: r#"trigger.id + ":run""#,
//!     model_name: "my-job",
//!     python: &config,
//!     inputs: vec![JobInput::inline("data", "trigger.data")],
//!     outputs: vec![JobOutput::new("result")],
//! }.wire(ctx);
//! ```

use crate::context::Context;
use crate::effect_tokens::SchedulerSubmitInput;
use crate::place::PlaceHandle;
use crate::token::Token;

/// Configuration for a Python executor job.
pub struct PythonJobConfig {
    /// Python script content (typically from `include_str!`).
    ///
    /// When using [`PythonJobDispatch::wire`], this is auto-registered as a Rhai variable
    /// and prepended as a raw input — no manual `ctx.rhai_var()` needed.
    ///
    /// When using the lower-level [`python_job_rhai`], register manually via `ctx.rhai_var()`
    /// and pass as [`JobInput::script`].
    ///
    /// **Prefer `script_local_path`** for new code — it stages the script to the
    /// artifact store and avoids Rhai scope pollution.
    pub script_content: String,
    /// Filename for the script in the executor workspace (e.g., `"fit_gp.py"`).
    pub script_filename: String,
    /// Local filesystem path to the Python script (e.g., `"./python/fit_gp.py"`).
    ///
    /// When set, [`PythonJobDispatch::wire`] stages the file via
    /// [`Context::stage_file`] and references it by `storage_path` instead of
    /// embedding content as a Rhai variable. This avoids polluting every
    /// transition's Rhai scope with script content.
    ///
    /// Takes precedence over `script_content` when both are set.
    pub script_local_path: Option<String>,
    /// pip requirements (e.g., `["numpy", "scipy"]`).
    pub requirements: Vec<String>,
    /// Max retries before dead-lettering.
    pub max_retries: i64,
    /// Whether to create a virtualenv.
    pub virtualenv: bool,
    /// Whether to use the aithericon Python SDK.
    pub sdk: bool,
    /// Python binary path override (e.g., a shared venv's python3).
    /// When set, emits `python: "<path>"` in the executor config.
    pub python: Option<String>,
    /// Event categories to stream in real-time during execution.
    /// When set, emits `stream_events: [...]` in the spec (sibling of `backend`/`config`).
    /// The executor extracts this and publishes matching IPC events to NATS JetStream.
    /// Valid categories: `"metric"`, `"progress"`, `"phase"`, `"log"`.
    pub stream_events: Option<Vec<String>>,
    /// Nix packages for the execution environment.
    /// When set, the executor's NixEnvironmentHook resolves a cached, content-addressed
    /// Nix environment instead of relying on a pre-built venv or system Python.
    /// Example: `["python311", "python311Packages.numpy", "python311Packages.scipy"]`
    pub nix_packages: Option<Vec<String>>,
}

/// An input declaration for the Python job spec.
pub enum JobInput {
    /// Inline JSON value. `value_expr` is a Rhai expression producing the value.
    Inline { name: String, value_expr: String },
    /// Raw text content. `content_expr` is a Rhai expression producing a string.
    /// Used for script files — written verbatim, not JSON-serialized.
    Raw { name: String, content_expr: String },
    /// Storage path reference (uses the executor's global artifact store).
    StoragePath { name: String, path_expr: String },
}

impl JobInput {
    /// Create an inline JSON input with a Rhai expression for the value.
    pub fn inline(name: impl Into<String>, value_expr: impl Into<String>) -> Self {
        Self::Inline {
            name: name.into(),
            value_expr: value_expr.into(),
        }
    }

    /// Create a raw text input (e.g., a script file). Written verbatim, not JSON-serialized.
    pub fn raw(name: impl Into<String>, content_expr: impl Into<String>) -> Self {
        Self::Raw {
            name: name.into(),
            content_expr: content_expr.into(),
        }
    }

    /// Create a raw input for a Python script file.
    ///
    /// Semantically identical to [`raw`](Self::raw), but communicates intent: `var_name`
    /// should reference a Rhai variable registered via `ctx.rhai_var()` containing the
    /// script source. Use [`script_var_name`] to derive a safe variable name from the filename.
    pub fn script(filename: impl Into<String>, var_name: impl Into<String>) -> Self {
        Self::Raw {
            name: filename.into(),
            content_expr: var_name.into(),
        }
    }

    /// Create a storage path input (downloaded from the global artifact store).
    ///
    /// `path_expr` is a **Rhai expression** that evaluates to the storage path
    /// string. For dynamic paths use a Rhai variable (e.g., `"candidate.model_path"`).
    /// For static paths, prefer [`storage_path_literal`](Self::storage_path_literal).
    pub fn storage_path(name: impl Into<String>, path_expr: impl Into<String>) -> Self {
        Self::StoragePath {
            name: name.into(),
            path_expr: path_expr.into(),
        }
    }

    /// Create a storage path input with a static path string.
    ///
    /// Unlike [`storage_path`](Self::storage_path), this takes a plain Rust string
    /// and wraps it as a Rhai string literal automatically.
    ///
    /// # Example
    /// ```ignore
    /// JobInput::storage_path_literal("fit_gp.py", "scripts/fit_gp.py")
    /// // Generates: path: "scripts/fit_gp.py" in the Rhai output
    /// ```
    pub fn storage_path_literal(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self::StoragePath {
            name: name.into(),
            path_expr: format!("\"{}\"", path.into()),
        }
    }
}

/// An output declaration for the Python job spec.
pub struct JobOutput {
    pub name: String,
}

impl JobOutput {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// Generate the Rhai map literal for a Python job dispatch.
///
/// Returns a Rhai script fragment producing `#{ job: #{...}, pending: <input_var> }`.
/// The caller is responsible for wiring the transition and providing the input variable in scope.
///
/// `input_var` is the Rhai variable name for the trigger token (default: `"trigger"`).
/// Must match the `auto_input` port name on the transition.
///
/// The script content variable (e.g., `MY_SCRIPT`) must be defined in the Rhai
/// preamble via `ctx.rhai_var()`. Pass the **variable name** as an entry in `inputs`
/// using [`JobInput::Inline`] with the variable name as `value_expr`.
pub fn python_job_rhai(
    job_id_expr: &str,
    model_name: &str,
    config: &PythonJobConfig,
    inputs: &[JobInput],
    outputs: &[JobOutput],
) -> String {
    python_job_rhai_with_var("trigger", job_id_expr, model_name, config, inputs, outputs)
}

/// Like [`python_job_rhai`], but with a custom input variable name.
pub fn python_job_rhai_with_var(
    input_var: &str,
    job_id_expr: &str,
    model_name: &str,
    config: &PythonJobConfig,
    inputs: &[JobInput],
    outputs: &[JobOutput],
) -> String {
    python_job_rhai_with_template(
        input_var, job_id_expr, model_name, None, config, inputs, outputs,
    )
}

/// Like [`python_job_rhai_with_var`], but emits an optional per-job scheduler
/// template override.
///
/// When `template_id` is `Some`, the generated Rhai includes a
/// `job_template_id: "..."` field on the outer `SchedulerSubmitInput` map.
/// The scheduler handler uses this to route the job to a specific Nomad
/// parameterized template (e.g., a GPU-enabled template) instead of the
/// handler's configured default.
///
/// Pass `None` for standard dispatch to the handler's default template.
pub fn python_job_rhai_with_template(
    input_var: &str,
    job_id_expr: &str,
    model_name: &str,
    template_id: Option<&str>,
    config: &PythonJobConfig,
    inputs: &[JobInput],
    outputs: &[JobOutput],
) -> String {
    let inputs_rhai = inputs
        .iter()
        .map(|input| match input {
            JobInput::Inline { name, value_expr } => {
                format!(
                    r#"#{{ name: "{name}", source: #{{ "type": "inline", value: {value_expr} }} }}"#
                )
            }
            JobInput::Raw { name, content_expr } => {
                format!(
                    r#"#{{ name: "{name}", source: #{{ "type": "raw", content: {content_expr} }} }}"#
                )
            }
            JobInput::StoragePath { name, path_expr } => {
                format!(
                    r#"#{{ name: "{name}", source: #{{ "type": "storage_path", path: {path_expr} }} }}"#
                )
            }
        })
        .collect::<Vec<_>>()
        .join(",\n                    ");

    let outputs_rhai = outputs
        .iter()
        .map(|o| format!(r#"#{{ name: "{}" }}"#, o.name))
        .collect::<Vec<_>>()
        .join(", ");

    let reqs_rhai = if config.requirements.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[{}]",
            config
                .requirements
                .iter()
                .map(|r| format!("\"{r}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let template_field = template_id.map_or(String::new(), |t| {
        format!(",\n                    job_template_id: \"{t}\"")
    });

    format!(
        r#"#{{
                job: #{{
                    job_id: {job_id},
                    model_name: "{model_name}",
                    run: 0,
                    retries: 0,
                    max_retries: {max_retries}{template_field},
                    spec: #{{
                        backend: "python",
                        config: #{{
                            script: "{script_filename}",
                            virtualenv: {virtualenv},
                            sdk: {sdk},
                            requirements: {requirements}{python_field}{nix_field}
                        }},
                        inputs: [
                    {inputs}
                        ],
                        outputs: [{outputs}]{stream_events_field}
                    }}
                }},
                pending: {input_var}
            }}"#,
        input_var = input_var,
        job_id = job_id_expr,
        model_name = model_name,
        max_retries = config.max_retries,
        template_field = template_field,
        script_filename = config.script_filename,
        virtualenv = config.virtualenv,
        sdk = config.sdk,
        requirements = reqs_rhai,
        python_field = config.python.as_ref().map_or(String::new(), |p| format!(",\n                            python: \"{p}\"")),
        nix_field = config.nix_packages.as_ref().map_or(String::new(), |pkgs| {
            let items = pkgs.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(", ");
            format!(",\n                            nix: #{{ packages: [{items}] }}")
        }),
        inputs = inputs_rhai,
        outputs = outputs_rhai,
        stream_events_field = config.stream_events.as_ref().map_or(String::new(), |cats| {
            let items = cats.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
            format!(",\n                        stream_events: [{items}]")
        }),
    )
}

/// Derive a safe Rhai variable name from a script filename.
///
/// `"fit_gp.py"` → `"__script_fit_gp_py"`, `"my-script.py"` → `"__script_my_script_py"`.
pub fn script_var_name(filename: &str) -> String {
    let sanitized: String = filename
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    format!("__script_{sanitized}")
}

/// Configuration for dynamic (batch) inputs appended at runtime via a Rhai loop.
///
/// The generated Rhai iterates over `collection_var` (a batch-read variable),
/// extracts a storage path from each element, and appends `storage_path` inputs
/// to the static inputs array. The loop counter is exposed as `__dyn_count` for
/// use in other expressions (e.g., `job_id`, inline input values).
pub struct DynamicInputs<'a> {
    /// Rhai variable name bound via `read_input_batch()` (e.g., `"observations"`).
    pub collection_var: &'a str,
    /// Filename prefix for generated inputs (e.g., `"obs_"` produces `obs_0`, `obs_1`, ...).
    pub name_prefix: &'a str,
    /// Rhai expression to extract the storage path from each element.
    /// The loop variable is `__item`. Example: `"__item.payload.artifact.storage_path"`.
    pub path_expr: &'a str,
    /// Optional Rhai guard. Only items where this evaluates to true are included.
    /// The loop variable is `__item`. Example: `"__item.payload.artifact != ()"`.
    pub filter_expr: Option<&'a str>,
}

/// Like [`python_job_rhai_with_var`], but with dynamic batch inputs appended via a Rhai loop.
///
/// The generated code:
/// 1. Builds a static inputs array from `inputs`
/// 2. Iterates over `dynamic.collection_var`, appending `storage_path` entries
/// 3. Merges them into `__all_inputs`
/// 4. Uses `__all_inputs` in the job spec
///
/// The loop counter `__dyn_count` is available in `job_id_expr` and input value expressions.
pub fn python_job_rhai_with_dynamic(
    input_var: &str,
    job_id_expr: &str,
    model_name: &str,
    config: &PythonJobConfig,
    inputs: &[JobInput],
    outputs: &[JobOutput],
    dynamic: &DynamicInputs,
) -> String {
    let static_inputs_rhai = inputs
        .iter()
        .map(|input| match input {
            JobInput::Inline { name, value_expr } => {
                format!(
                    r#"#{{ name: "{name}", source: #{{ "type": "inline", value: {value_expr} }} }}"#
                )
            }
            JobInput::Raw { name, content_expr } => {
                format!(
                    r#"#{{ name: "{name}", source: #{{ "type": "raw", content: {content_expr} }} }}"#
                )
            }
            JobInput::StoragePath { name, path_expr } => {
                format!(
                    r#"#{{ name: "{name}", source: #{{ "type": "storage_path", path: {path_expr} }} }}"#
                )
            }
        })
        .collect::<Vec<_>>()
        .join(",\n                    ");

    let outputs_rhai = outputs
        .iter()
        .map(|o| format!(r#"#{{ name: "{}" }}"#, o.name))
        .collect::<Vec<_>>()
        .join(", ");

    let reqs_rhai = if config.requirements.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[{}]",
            config
                .requirements
                .iter()
                .map(|r| format!("\"{r}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let filter_guard = dynamic.filter_expr.map_or(String::new(), |expr| {
        format!("\n            if {expr} {{")
    });
    let filter_close = if dynamic.filter_expr.is_some() {
        "\n            }"
    } else {
        ""
    };

    format!(
        r#"let __dyn_inputs = [];
        let __dyn_count = 0;
        for __item in {collection_var} {{{filter_guard}
                __dyn_inputs.push(#{{
                    name: "{name_prefix}" + __dyn_count,
                    source: #{{ "type": "storage_path", path: {path_expr} }},
                    required: true
                }});
                __dyn_count += 1;{filter_close}
        }}
        let __static_inputs = [
                    {static_inputs}
                ];
        let __all_inputs = __static_inputs;
        for __d in __dyn_inputs {{ __all_inputs.push(__d); }}
        #{{
                job: #{{
                    job_id: {job_id},
                    model_name: "{model_name}",
                    run: 0,
                    retries: 0,
                    max_retries: {max_retries},
                    spec: #{{
                        backend: "python",
                        config: #{{
                            script: "{script_filename}",
                            virtualenv: {virtualenv},
                            sdk: {sdk},
                            requirements: {requirements}{python_field}{nix_field}
                        }},
                        inputs: __all_inputs,
                        outputs: [{outputs}]{stream_events_field}
                    }}
                }},
                pending: {input_var}
            }}"#,
        static_inputs = static_inputs_rhai,
        collection_var = dynamic.collection_var,
        filter_guard = filter_guard,
        filter_close = filter_close,
        name_prefix = dynamic.name_prefix,
        path_expr = dynamic.path_expr,
        input_var = input_var,
        job_id = job_id_expr,
        model_name = model_name,
        max_retries = config.max_retries,
        script_filename = config.script_filename,
        virtualenv = config.virtualenv,
        sdk = config.sdk,
        requirements = reqs_rhai,
        python_field = config.python.as_ref().map_or(String::new(), |p| format!(",\n                            python: \"{p}\"")),
        nix_field = config.nix_packages.as_ref().map_or(String::new(), |pkgs| {
            let items = pkgs.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(", ");
            format!(",\n                            nix: #{{ packages: [{items}] }}")
        }),
        outputs = outputs_rhai,
        stream_events_field = config.stream_events.as_ref().map_or(String::new(), |cats| {
            let items = cats.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
            format!(",\n                        stream_events: [{items}]")
        }),
    )
}

/// Full-wiring Python job dispatch.
///
/// Creates a dispatch transition consuming from `trigger`, producing to `job_queue`
/// (bridge-out to job-net) and a new `pending` place. Returns the pending place handle
/// for result correlation.
///
/// When `python.script_content` is non-empty, `wire()` automatically registers the
/// script as a Rhai variable and prepends a raw input for it. No manual `ctx.rhai_var()`
/// or `JobInput::script()` call is needed.
pub struct PythonJobDispatch<'a, T: Token> {
    /// Transition ID (e.g., `"dispatch_fit"`).
    pub id: &'a str,
    /// Human-readable label.
    pub label: &'a str,
    /// Input place that triggers the dispatch.
    pub trigger: &'a PlaceHandle<T>,
    /// Bridge-out place to the job-net queue.
    pub job_queue: &'a PlaceHandle<SchedulerSubmitInput>,
    /// Rhai expression for job_id (e.g., `r#"trigger.campaign_id + ":fit-" + trigger.iteration"#`).
    pub job_id_expr: &'a str,
    /// Model name string literal.
    pub model_name: &'a str,
    /// Python configuration.
    pub python: &'a PythonJobConfig,
    /// Input declarations for the job spec.
    pub inputs: Vec<JobInput>,
    /// Output declarations for the job spec.
    pub outputs: Vec<JobOutput>,
}

impl<'a, T: Token> PythonJobDispatch<'a, T> {
    /// Wire the dispatch transition and create the pending place.
    ///
    /// Returns a `PlaceHandle<T>` for the pending place, which you'll use
    /// in a join transition to correlate with the job result.
    pub fn wire(mut self, ctx: &mut Context) -> PlaceHandle<T> {
        let pending_id = format!("{}_pending", self.id);
        let pending_name = format!("{} Pending", self.label);
        let pending = ctx.state::<T>(&pending_id, &pending_name);

        // Stage script as an artifact (preferred) or embed as Rhai variable (legacy)
        if let Some(local_path) = &self.python.script_local_path {
            // New path: stage file for upload, reference by storage_path
            let storage_path =
                ctx.stage_file(format!("scripts/{}", self.python.script_filename), local_path);
            self.inputs.insert(
                0,
                JobInput::storage_path_literal(&self.python.script_filename, &storage_path),
            );
        } else if !self.python.script_content.is_empty() {
            // Legacy path: embed script content as a Rhai variable
            let var_name = script_var_name(&self.python.script_filename);
            ctx.rhai_var(&var_name, &self.python.script_content);
            self.inputs.insert(
                0,
                JobInput::script(&self.python.script_filename, &var_name),
            );
        }

        let script = python_job_rhai(
            self.job_id_expr,
            self.model_name,
            self.python,
            &self.inputs,
            &self.outputs,
        );

        ctx.transition(self.id, self.label)
            .auto_input("trigger", self.trigger)
            .auto_output("job", self.job_queue)
            .auto_output("pending", &pending)
            .logic(&script);

        pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::validate_script;

    #[test]
    fn generated_rhai_is_valid_syntax() {
        let config = PythonJobConfig {
            script_content: "print('hello')".into(),
            script_filename: "test.py".into(),
            requirements: vec!["numpy".into()],
            max_retries: 2,
            virtualenv: true,
            sdk: true,
            python: None,
            stream_events: None,
            nix_packages: None,
            script_local_path: None,
        };

        let rhai = python_job_rhai(
            r#""test-job-1""#,
            "test-model",
            &config,
            &[
                JobInput::inline("data.json", r#"#{ key: "value" }"#),
                JobInput::storage_path("model.json", r#""artifacts/model.json""#),
            ],
            &[JobOutput::new("result")],
        );

        // Wrap in a let binding so it's a complete Rhai statement
        let full_script = format!("let trigger = #{{ id: \"x\" }};\n{}", rhai);
        assert!(
            validate_script(&full_script).is_ok(),
            "Generated Rhai failed syntax check: {}",
            rhai
        );
    }

    #[test]
    fn generated_rhai_contains_expected_fields() {
        let config = PythonJobConfig {
            script_content: String::new(),
            script_filename: "my_script.py".into(),
            requirements: vec!["scipy".into(), "pandas".into()],
            max_retries: 3,
            virtualenv: false,
            sdk: true,
            python: None,
            stream_events: None,
            nix_packages: None,
            script_local_path: None,
        };

        let rhai = python_job_rhai(
            r#"trigger.id"#,
            "gp-fit",
            &config,
            &[JobInput::inline("obs", "trigger.observations")],
            &[JobOutput::new("model_meta")],
        );

        assert!(rhai.contains(r#"model_name: "gp-fit""#));
        assert!(rhai.contains("max_retries: 3"));
        assert!(rhai.contains(r#"script: "my_script.py""#));
        assert!(rhai.contains("virtualenv: false"));
        assert!(rhai.contains(r#"["scipy", "pandas"]"#));
        assert!(rhai.contains(r#"name: "obs""#));
        assert!(rhai.contains(r#"name: "model_meta""#));
        assert!(rhai.contains("pending: trigger"));
    }

    #[test]
    fn empty_requirements_produces_empty_array() {
        let config = PythonJobConfig {
            script_content: String::new(),
            script_filename: "test.py".into(),
            requirements: vec![],
            max_retries: 0,
            virtualenv: true,
            sdk: false,
            python: None,
            stream_events: None,
            nix_packages: None,
            script_local_path: None,
        };

        let rhai = python_job_rhai(r#""id""#, "test", &config, &[], &[]);
        assert!(rhai.contains("requirements: []"));
    }

    #[test]
    fn stream_events_emitted_when_set() {
        let config = PythonJobConfig {
            script_content: String::new(),
            script_filename: "test.py".into(),
            requirements: vec![],
            max_retries: 0,
            virtualenv: false,
            sdk: true,
            python: None,
            stream_events: Some(vec!["metric".into(), "progress".into(), "phase".into()]),
            nix_packages: None,
            script_local_path: None,
        };

        let rhai = python_job_rhai(r#""id""#, "test", &config, &[], &[]);
        assert!(
            rhai.contains(r#"stream_events: ["metric", "progress", "phase"]"#),
            "stream_events not found in generated Rhai: {}",
            rhai
        );

        // Validate the generated Rhai is valid syntax
        let full_script = format!("let trigger = #{{ id: \"x\" }};\n{}", rhai);
        assert!(
            validate_script(&full_script).is_ok(),
            "Generated Rhai with stream_events failed syntax check: {}",
            rhai
        );
    }

    #[test]
    fn script_var_name_from_filename() {
        assert_eq!(script_var_name("fit_gp.py"), "__script_fit_gp_py");
        assert_eq!(script_var_name("my-script.py"), "__script_my_script_py");
        assert_eq!(
            script_var_name("path/to/deep.py"),
            "__script_path_to_deep_py"
        );
        assert_eq!(script_var_name("simple"), "__script_simple");
    }

    #[test]
    fn job_input_script_same_as_raw() {
        let script = JobInput::script("fit.py", "MY_VAR");
        let raw = JobInput::raw("fit.py", "MY_VAR");
        match (&script, &raw) {
            (
                JobInput::Raw {
                    name: n1,
                    content_expr: c1,
                },
                JobInput::Raw {
                    name: n2,
                    content_expr: c2,
                },
            ) => {
                assert_eq!(n1, n2);
                assert_eq!(c1, c2);
            }
            _ => panic!("script() should produce Raw variant"),
        }
    }

    #[test]
    fn template_id_omitted_when_none() {
        let config = PythonJobConfig {
            script_content: String::new(),
            script_filename: "test.py".into(),
            requirements: vec![],
            max_retries: 1,
            virtualenv: false,
            sdk: true,
            python: None,
            stream_events: None,
            nix_packages: None,
            script_local_path: None,
        };

        let rhai = python_job_rhai_with_template(
            "trigger",
            r#""id""#,
            "m",
            None,
            &config,
            &[],
            &[],
        );
        assert!(
            !rhai.contains("job_template_id"),
            "job_template_id should be absent when None: {}",
            rhai
        );
    }

    #[test]
    fn template_id_emitted_when_set() {
        let config = PythonJobConfig {
            script_content: String::new(),
            script_filename: "test.py".into(),
            requirements: vec![],
            max_retries: 1,
            virtualenv: false,
            sdk: true,
            python: None,
            stream_events: None,
            nix_packages: None,
            script_local_path: None,
        };

        let rhai = python_job_rhai_with_template(
            "trigger",
            r#""id""#,
            "m",
            Some("petri-mumax3-worker"),
            &config,
            &[],
            &[],
        );
        assert!(
            rhai.contains(r#"job_template_id: "petri-mumax3-worker""#),
            "job_template_id not found: {}",
            rhai
        );

        let full_script = format!("let trigger = #{{ id: \"x\" }};\n{}", rhai);
        assert!(
            validate_script(&full_script).is_ok(),
            "Rhai with template_id failed syntax check: {}",
            rhai
        );
    }

    #[test]
    fn dynamic_inputs_generates_valid_rhai() {
        let config = PythonJobConfig {
            script_content: String::new(),
            script_filename: "fit.py".into(),
            requirements: vec!["numpy".into()],
            max_retries: 1,
            virtualenv: false,
            sdk: true,
            python: None,
            stream_events: None,
            nix_packages: None,
            script_local_path: None,
        };

        let rhai = python_job_rhai_with_dynamic(
            "_trigger",
            r#""job-" + __dyn_count"#,
            "gp-fit",
            &config,
            &[JobInput::inline(
                "config.json",
                r#"#{ iteration: __dyn_count }"#,
            )],
            &[JobOutput::new("model_meta")],
            &DynamicInputs {
                collection_var: "observations",
                name_prefix: "obs_",
                path_expr: "__item.artifact.storage_path",
                filter_expr: Some(
                    "__item.artifact != () && __item.artifact.storage_path != ()",
                ),
            },
        );

        // Wrap with variable declarations for a complete Rhai program
        let full_script = format!(
            "let _trigger = #{{ id: \"x\" }};\nlet observations = [];\n{}",
            rhai
        );
        assert!(
            validate_script(&full_script).is_ok(),
            "Generated dynamic Rhai failed syntax check:\n{}",
            full_script
        );

        // Check structural elements
        assert!(rhai.contains("__static_inputs"));
        assert!(rhai.contains("__dyn_inputs"));
        assert!(rhai.contains("__dyn_count"));
        assert!(rhai.contains("for __item in observations"));
        assert!(rhai.contains("__all_inputs"));
        assert!(rhai.contains(r#"name: "obs_" + __dyn_count"#));
        assert!(rhai.contains(r#"inputs: __all_inputs"#));
        assert!(rhai.contains(r#"model_name: "gp-fit""#));
    }

    #[test]
    fn dynamic_inputs_without_filter() {
        let config = PythonJobConfig {
            script_content: String::new(),
            script_filename: "test.py".into(),
            requirements: vec![],
            max_retries: 0,
            virtualenv: false,
            sdk: false,
            python: None,
            stream_events: None,
            nix_packages: None,
            script_local_path: None,
        };

        let rhai = python_job_rhai_with_dynamic(
            "trigger",
            r#""id""#,
            "test",
            &config,
            &[],
            &[],
            &DynamicInputs {
                collection_var: "items",
                name_prefix: "file_",
                path_expr: "__item.path",
                filter_expr: None,
            },
        );

        let full_script = format!("let trigger = #{{ id: \"x\" }};\nlet items = [];\n{}", rhai);
        assert!(
            validate_script(&full_script).is_ok(),
            "Dynamic Rhai without filter failed syntax check:\n{}",
            full_script
        );

        // No filter guard should appear
        assert!(!rhai.contains("if __item"));
    }

    #[test]
    fn stream_events_omitted_when_none() {
        let config = PythonJobConfig {
            script_content: String::new(),
            script_filename: "test.py".into(),
            requirements: vec![],
            max_retries: 0,
            virtualenv: false,
            sdk: false,
            python: None,
            stream_events: None,
            nix_packages: None,
            script_local_path: None,
        };

        let rhai = python_job_rhai(r#""id""#, "test", &config, &[], &[]);
        assert!(
            !rhai.contains("stream_events"),
            "stream_events should not appear when None: {}",
            rhai
        );
    }
}
