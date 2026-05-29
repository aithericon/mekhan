pub mod backend_configs;
pub(crate) mod borrow;
mod compile;
mod error;
pub(crate) mod graph;
pub mod interface;
pub(crate) mod lower;
mod pyio;
pub(crate) mod human_task_refs;
pub(crate) mod placeholder_refs;
pub(crate) mod python_refs;
pub(crate) mod resource_binding;
pub mod resource_refs;
pub(crate) mod schema_refs;
pub(crate) mod rhai_gen;
pub mod rhai_scope;
pub mod subworkflow;
pub mod token_shape;
pub mod well_known;
pub(crate) mod validate;
mod wire;

pub use compile::{
    compile_to_air, compile_to_air_with_options, compile_to_scenario, CompileArtifacts,
    CompileOptions, ResolvedChild, SubWorkflowAir,
};
pub use error::{CompileError, CompileErrorView};
pub use interface::{InterfaceRegistry, NodeInterface, NodeKind, OutputKey};
pub use lower::{node_files_inline, node_files_storage_path, ConfigStorage};
pub use pyio::generate_py_io_files;
pub use subworkflow::{
    derive_child_io, make_child_callable, CHILD_FAIL_OUT, CHILD_INBOX, CHILD_REPLY_OUT,
};
pub use token_shape::{
    analyze as analyze_token_shapes, node_namespace_scopes, surface_types, ScopeEntry,
    ShapeDiagnostic, ShapeReport, TyDescriptor, TypeSurface,
};
pub use validate::{node_input_scopes, node_output_fields, resolve_trigger_target_port};
