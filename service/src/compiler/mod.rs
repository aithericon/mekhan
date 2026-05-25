pub mod backend_configs;
mod compile;
mod error;
mod graph;
pub mod interface;
mod lower;
mod pyio;
pub(crate) mod human_task_refs;
pub(crate) mod placeholder_refs;
pub(crate) mod python_refs;
mod rhai_gen;
pub mod rhai_scope;
pub mod subworkflow;
pub mod token_shape;
pub mod well_known;
mod validate;
mod wire;

pub use compile::{
    compile_to_air, compile_to_air_with_subworkflows, compile_to_air_with_subworkflows_inline,
    compile_to_air_with_subworkflows_and_interfaces, compile_to_scenario,
    compile_to_scenario_and_interfaces, compile_to_scenario_with_inline_sources, ResolvedChild,
    SubWorkflowAir,
};
pub use error::{CompileError, CompileErrorView};
pub use interface::{InterfaceRegistry, NodeInterface, NodeKind, OutputKey};
pub use lower::{node_files_inline, node_files_storage_path};
pub use pyio::generate_py_io_files;
pub use subworkflow::{make_child_callable, CHILD_FAIL_OUT, CHILD_INBOX, CHILD_REPLY_OUT};
pub use token_shape::{
    analyze as analyze_token_shapes, node_namespace_scopes, surface_types, ScopeEntry,
    ShapeDiagnostic, ShapeReport, TypeSurface,
};
pub use validate::{node_input_scopes, node_output_fields, resolve_trigger_target_port};
