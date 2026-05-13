//! # Aithericon SDK Derive Macros
//!
//! Provides procedural macros for the Petri net SDK:
//!
//! - `#[token]` - Define token types with automatic derive implementations
//! - `#[step]` - Define transition steps with functional syntax
//! - `#[guard]` - Add guard conditions to steps
//!
//! ## Token Macro
//!
//! ```ignore
//! use aithericon_sdk::prelude::*;
//!
//! #[token]
//! struct Job {
//!     id: String,
//!     priority: u32,
//! }
//! ```
//!
//! ## Step Macro (v3 - with Guards and Branching)
//!
//! ```ignore
//! use aithericon_sdk::prelude::*;
//!
//! // Simple step - single output
//! #[step("allocate", "Allocate Task")]
//! fn allocate(task: Task, worker: Worker) -> Assignment {
//!     Assignment { task_id: task.id, worker_id: worker.id }
//! }
//!
//! // Step with guard - conditional execution
//! #[step("handle_vip", "Handle VIP Task")]
//! #[guard("task.priority >= 10")]
//! fn handle_vip(task: Task) -> VipAssignment {
//!     VipAssignment { task_id: task.id }
//! }
//!
//! // Branching step - multiple outputs via tuple
//! #[step("review", "Review Work")]
//! fn review(work: Work) -> (Approved, Rejected) {
//!     // Rhai script decides which output
//!     r#"if work.quality > 80 { #{ approved: work } } else { #{ rejected: work } }"#
//! }
//! ```

mod rhai_gen;

use heck::{ToPascalCase, ToSnakeCase};
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input, Attribute, FnArg, ItemFn, ItemStruct, LitStr, Pat, ReturnType, Token, Type,
};

/// Attribute macro for defining Petri net token types.
///
/// Automatically adds `#[derive(Clone, Debug, Serialize, JsonSchema)]` to the struct.
///
/// # Example
///
/// ```ignore
/// use aithericon_sdk::prelude::*;
///
/// #[token]
/// struct Task {
///     id: String,
///     name: String,
/// }
///
/// #[token]
/// struct Worker {
///     id: String,
///     skill: String,
/// }
/// ```
///
/// You can also add additional derives:
///
/// ```ignore
/// #[token]
/// #[derive(PartialEq, Eq, Hash)]
/// struct ResourceId {
///     id: String,
/// }
/// ```
#[proc_macro_attribute]
pub fn token(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);

    let attrs = &input.attrs;
    let vis = &input.vis;
    let name = &input.ident;
    let generics = &input.generics;
    let fields = &input.fields;
    let semi_token = &input.semi_token;

    let expanded = quote! {
        #[derive(Clone, Debug, ::serde::Serialize, ::schemars::JsonSchema)]
        #(#attrs)*
        #vis struct #name #generics #fields #semi_token
    };

    expanded.into()
}

/// Arguments for the `#[step]` attribute.
/// Format: `#[step("id", "Display Name")]`
struct StepArgs {
    id: LitStr,
    name: LitStr,
}

impl Parse for StepArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let id: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let name: LitStr = input.parse()?;
        Ok(StepArgs { id, name })
    }
}

/// Distinguishes regular input params from Target<T> output params.
#[derive(Clone, Copy, PartialEq)]
enum ParamKind {
    Input,  // Regular input from a place
    Target, // Output to an existing place (Target<T>)
}

/// Parameter info extracted from function signature.
struct ParamInfo {
    name: String,
    type_name: String, // The type name (inner type for Target<T>)
    kind: ParamKind,
    full_type: Box<Type>, // The full type AST for codegen
}

/// Check if a type is `Target<T>` and extract the inner type.
/// Returns (is_target, inner_type, inner_type_name)
fn extract_target_type(ty: &Type) -> Option<(Box<Type>, String)> {
    if let Type::Path(type_path) = ty {
        if let Some(seg) = type_path.path.segments.last() {
            if seg.ident == "Target" {
                // Extract inner type from Target<T>
                if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                    if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                        let inner_name = extract_type_name(inner);
                        return Some((Box::new(inner.clone()), inner_name));
                    }
                }
            }
        }
    }
    None
}

/// Helper to extract guard expression from attributes
fn extract_guard(attrs: &[Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("guard") {
            // Parse the attribute as #[guard("expression")]
            if let Ok(lit) = attr.parse_args::<LitStr>() {
                return Some(lit.value());
            }
        }
    }
    None
}

/// Information about a single output type
struct OutputInfo {
    name: String,      // snake_case name for the port
    type_name: String, // PascalCase type name
    ty: Box<Type>,     // The actual Type for codegen
}

/// Extract output information from return type.
/// Supports both single types and tuples for branching.
fn extract_outputs(return_type: &ReturnType) -> Result<Vec<OutputInfo>, String> {
    match return_type {
        ReturnType::Type(_, ty) => {
            match ty.as_ref() {
                // Tuple type: (A, B, C) -> multiple outputs
                Type::Tuple(tuple) => {
                    let outputs: Vec<OutputInfo> = tuple
                        .elems
                        .iter()
                        .map(|elem| {
                            let type_name = extract_type_name(elem);
                            OutputInfo {
                                name: type_name.to_snake_case(),
                                type_name,
                                ty: Box::new(elem.clone()),
                            }
                        })
                        .collect();

                    if outputs.is_empty() {
                        return Err("Empty tuple return type not supported".to_string());
                    }
                    Ok(outputs)
                }
                // Single type
                _ => {
                    let type_name = extract_type_name(ty);
                    Ok(vec![OutputInfo {
                        name: type_name.to_snake_case(),
                        type_name,
                        ty: ty.clone(),
                    }])
                }
            }
        }
        ReturnType::Default => Err("Step function must have a return type".to_string()),
    }
}

/// Attribute macro for defining transition steps with functional syntax.
///
/// # Features (v3)
///
/// 1. **Unique IDs**: Each call generates unique place/transition IDs to prevent collisions
/// 2. **Guards**: Use `#[guard("expression")]` for conditional execution
/// 3. **Branching**: Return tuples `(A, B)` to create multiple output paths
///
/// # Arguments
///
/// - First argument: The transition ID (base identifier, will be made unique)
/// - Second argument: The human-readable transition name
///
/// # Examples
///
/// ## Simple Step
/// ```ignore
/// #[step("allocate", "Allocate Task")]
/// fn allocate(task: Task, worker: Worker) -> Assignment {
///     Assignment { task_id: task.id, worker_id: worker.id }
/// }
///
/// // Can be called multiple times - unique IDs generated!
/// let assign1 = allocate(ctx, &tasks, &workers);  // allocate_1__assignment
/// let assign2 = allocate(ctx, &tasks, &workers);  // allocate_2__assignment
/// ```
///
/// ## Step with Guard
/// ```ignore
/// #[step("handle_vip", "Handle VIP")]
/// #[guard("task.priority >= 10")]
/// fn handle_vip(task: Task) -> VipResult {
///     VipResult { task_id: task.id }
/// }
/// ```
///
/// ## Branching Step (Multiple Outputs)
/// ```ignore
/// #[step("review", "Review Work")]
/// fn review(work: Work) -> (Approved, Rejected) {
///     // Body must handle both outputs in Rhai
/// }
///
/// // Returns tuple of handles
/// let (approved, rejected) = review(ctx, &work);
/// ```
#[proc_macro_attribute]
pub fn step(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as StepArgs);
    let input = parse_macro_input!(item as ItemFn);

    // Extract function info
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();

    // Generate step struct name (e.g., `allocate` -> `AllocateStep`)
    let step_name = format_ident!("{}Step", fn_name_str.to_pascal_case());

    // Extract guard from attributes
    let guard_expr = extract_guard(&input.attrs);

    // Extract parameters - distinguish between Input and Target<T> params
    let params: Vec<ParamInfo> = input
        .sig
        .inputs
        .iter()
        .filter_map(|arg| {
            if let FnArg::Typed(pat_type) = arg {
                if let Pat::Ident(pat_ident) = &*pat_type.pat {
                    let param_name = pat_ident.ident.to_string();

                    // Check if this is a Target<T> param
                    if let Some((inner_type, inner_type_name)) = extract_target_type(&pat_type.ty) {
                        return Some(ParamInfo {
                            name: param_name,
                            type_name: inner_type_name,
                            kind: ParamKind::Target,
                            full_type: inner_type,
                        });
                    }

                    // Regular input param
                    let type_name = extract_type_name(&pat_type.ty);
                    return Some(ParamInfo {
                        name: param_name,
                        type_name,
                        kind: ParamKind::Input,
                        full_type: Box::new((*pat_type.ty).clone()),
                    });
                }
            }
            None
        })
        .collect();

    // Separate inputs from targets
    let input_params: Vec<&ParamInfo> = params
        .iter()
        .filter(|p| p.kind == ParamKind::Input)
        .collect();
    let target_params: Vec<&ParamInfo> = params
        .iter()
        .filter(|p| p.kind == ParamKind::Target)
        .collect();

    // Check if step has target outputs (for cyclic flows)
    let has_targets = !target_params.is_empty();

    // Extract output(s) - supports both single type and tuples
    // For steps with only Target outputs and no return type, outputs may be empty
    let outputs = match extract_outputs(&input.sig.output) {
        Ok(outputs) => outputs,
        Err(e) => {
            // If we have targets and no return type, that's OK
            if has_targets {
                vec![] // No new places created
            } else {
                return syn::Error::new_spanned(&input.sig, e)
                    .to_compile_error()
                    .into();
            }
        }
    };

    let is_branching = outputs.len() > 1;

    // Convert function body to Rhai script
    // For target outputs, we need to pass the target param names to rhai_gen
    let target_names: Vec<String> = target_params.iter().map(|p| p.name.clone()).collect();
    let rhai_script = if has_targets && outputs.is_empty() {
        // Step has only Target outputs - use target param name as output key
        match rhai_gen::rust_block_to_rhai_with_targets(&input.block, &target_names) {
            Ok(script) => script,
            Err(e) => {
                return syn::Error::new_spanned(&input.block, e)
                    .to_compile_error()
                    .into();
            }
        }
    } else {
        match rhai_gen::rust_block_to_rhai(&input.block) {
            Ok(script) => script,
            Err(e) => {
                return syn::Error::new_spanned(&input.block, e)
                    .to_compile_error()
                    .into();
            }
        }
    };

    // Generate metadata for inputs only (targets are outputs)
    let input_names: Vec<_> = input_params.iter().map(|p| p.name.as_str()).collect();
    let input_types: Vec<_> = input_params.iter().map(|p| p.type_name.as_str()).collect();

    // Generate function parameters: inputs use generic Ti, targets use PlaceHandle<InnerType>
    let fn_input_param_names: Vec<_> = input_params
        .iter()
        .map(|p| format_ident!("{}", p.name))
        .collect();

    let fn_input_param_generics: Vec<_> = input_params
        .iter()
        .enumerate()
        .map(|(i, _)| format_ident!("T{}", i))
        .collect();

    let fn_target_param_names: Vec<_> = target_params
        .iter()
        .map(|p| format_ident!("{}", p.name))
        .collect();

    let fn_target_param_types: Vec<_> = target_params.iter().map(|p| &p.full_type).collect();

    // Generate auto_input calls (only for input params)
    let auto_input_calls: Vec<_> = input_params
        .iter()
        .map(|p| {
            let name = &p.name;
            let name_ident = format_ident!("{}", p.name);
            quote! { .auto_input(#name, #name_ident) }
        })
        .collect();

    // Generate auto_output calls for target params (wire to existing places)
    let auto_target_output_calls: Vec<_> = target_params
        .iter()
        .map(|p| {
            let name = &p.name;
            let name_ident = format_ident!("{}", p.name);
            quote! { .auto_output(#name, #name_ident) }
        })
        .collect();

    // Base IDs and names
    let step_id = args.id.value();
    let step_display_name = args.name.value();

    // Generate guard call if present
    let guard_call = guard_expr.as_ref().map(|expr| {
        quote! { .guard(#expr) }
    });

    // Generate code based on output configuration:
    // 1. Only Target outputs (no return type) - cyclic flows
    // 2. Multiple return outputs (branching) - may also have targets
    // 3. Single return output - may also have targets
    let expanded = if has_targets && outputs.is_empty() {
        // CASE 1: TARGET-ONLY - Step outputs to existing places only (cyclic flows)
        // Function returns () and accepts target place handles
        let target_param_names_str: Vec<_> =
            target_params.iter().map(|p| p.name.as_str()).collect();
        let target_param_types_str: Vec<_> =
            target_params.iter().map(|p| p.type_name.as_str()).collect();

        // For StepDefinition::output(), get first target
        let first_target_name = target_params.first().map(|p| p.name.as_str()).unwrap_or("");
        let first_target_type = target_params
            .first()
            .map(|p| p.type_name.as_str())
            .unwrap_or("");

        quote! {
            /// Functional step generated by `#[step]` macro for cyclic flows.
            /// Outputs to existing place(s) via Target<T> parameters.
            pub fn #fn_name<#(#fn_input_param_generics: aithericon_sdk::token::Token,)*>(
                ctx: &mut aithericon_sdk::Context,
                #(#fn_input_param_names: &aithericon_sdk::PlaceHandle<#fn_input_param_generics>,)*
                #(#fn_target_param_names: &aithericon_sdk::PlaceHandle<#fn_target_param_types>,)*
            ) {
                // Generate unique instance ID
                let instance_id = ctx.next_step_id(#step_id);

                // Wire the transition - no new places created
                ctx.transition(&instance_id, #step_display_name)
                    #(#auto_input_calls)*
                    #(#auto_target_output_calls)*
                    #guard_call
                    .logic(#rhai_script);
            }

            /// Step metadata struct generated by `#[step]` macro.
            pub struct #step_name;

            impl aithericon_sdk::StepDefinition for #step_name {
                fn id() -> &'static str {
                    #step_id
                }

                fn name() -> &'static str {
                    #step_display_name
                }

                fn inputs() -> &'static [(&'static str, &'static str)] {
                    &[#((#input_names, #input_types)),*]
                }

                fn output() -> (&'static str, &'static str) {
                    // For target-only steps, report the first target
                    (#first_target_name, #first_target_type)
                }

                fn script() -> &'static str {
                    #rhai_script
                }
            }

            impl #step_name {
                /// Get target output port names and types.
                pub fn targets() -> &'static [(&'static str, &'static str)] {
                    &[#((#target_param_names_str, #target_param_types_str)),*]
                }
            }
        }
    } else if is_branching {
        // CASE 2: BRANCHING - Multiple outputs via tuple (may also have targets)
        let output_types: Vec<_> = outputs.iter().map(|o| &o.ty).collect();
        let output_names: Vec<_> = outputs.iter().map(|o| o.name.as_str()).collect();
        let output_type_strs: Vec<_> = outputs.iter().map(|o| o.type_name.as_str()).collect();
        let output_idents: Vec<_> = outputs
            .iter()
            .map(|o| format_ident!("output_{}", o.name))
            .collect();

        // Generate output place creations
        let output_place_creations: Vec<_> = outputs
            .iter()
            .zip(output_idents.iter())
            .map(|(o, ident)| {
                let ty = &o.ty;
                let name = &o.name;
                let type_str = &o.type_name;
                quote! {
                    let #ident = ctx.state::<#ty>(
                        format!("{}__{}", &instance_id, #name),
                        format!("{} → {}", #step_display_name, #type_str)
                    );
                }
            })
            .collect();

        // Generate auto_output calls for new places
        let auto_output_calls: Vec<_> = outputs
            .iter()
            .zip(output_idents.iter())
            .map(|(o, ident)| {
                let name = &o.name;
                quote! { .auto_output(#name, &#ident) }
            })
            .collect();

        // Output metadata for StepDefinition
        let first_output_name = &outputs[0].name;
        let first_output_type = &outputs[0].type_name;

        quote! {
            /// Functional branching step generated by `#[step]` macro.
            /// Creates multiple output places and returns tuple of handles.
            pub fn #fn_name<#(#fn_input_param_generics: aithericon_sdk::token::Token,)*>(
                ctx: &mut aithericon_sdk::Context,
                #(#fn_input_param_names: &aithericon_sdk::PlaceHandle<#fn_input_param_generics>,)*
                #(#fn_target_param_names: &aithericon_sdk::PlaceHandle<#fn_target_param_types>,)*
            ) -> (#(aithericon_sdk::PlaceHandle<#output_types>,)*) {
                // Generate unique instance ID
                let instance_id = ctx.next_step_id(#step_id);

                // Create output places
                #(#output_place_creations)*

                // Wire the transition
                ctx.transition(&instance_id, #step_display_name)
                    #(#auto_input_calls)*
                    #(#auto_output_calls)*
                    #(#auto_target_output_calls)*
                    #guard_call
                    .logic(#rhai_script);

                // Return tuple of handles
                (#(#output_idents,)*)
            }

            /// Step metadata struct generated by `#[step]` macro.
            pub struct #step_name;

            impl aithericon_sdk::StepDefinition for #step_name {
                fn id() -> &'static str {
                    #step_id
                }

                fn name() -> &'static str {
                    #step_display_name
                }

                fn inputs() -> &'static [(&'static str, &'static str)] {
                    &[#((#input_names, #input_types)),*]
                }

                fn output() -> (&'static str, &'static str) {
                    // For branching, return first output (use outputs() for all)
                    (#first_output_name, #first_output_type)
                }

                fn script() -> &'static str {
                    #rhai_script
                }
            }

            impl #step_name {
                /// Get all output port names and types for branching steps.
                pub fn outputs() -> &'static [(&'static str, &'static str)] {
                    &[#((#output_names, #output_type_strs)),*]
                }
            }
        }
    } else {
        // CASE 3: SINGLE OUTPUT - Original behavior with unique IDs (may also have targets)
        let output = &outputs[0];
        let output_name = &output.name;
        let output_type_str = &output.type_name;
        let output_type = &output.ty;

        quote! {
            /// Functional step generated by `#[step]` macro.
            /// Creates output place internally and returns handle for chaining.
            pub fn #fn_name<#(#fn_input_param_generics: aithericon_sdk::token::Token,)*>(
                ctx: &mut aithericon_sdk::Context,
                #(#fn_input_param_names: &aithericon_sdk::PlaceHandle<#fn_input_param_generics>,)*
                #(#fn_target_param_names: &aithericon_sdk::PlaceHandle<#fn_target_param_types>,)*
            ) -> aithericon_sdk::PlaceHandle<#output_type> {
                // Generate unique instance ID
                let instance_id = ctx.next_step_id(#step_id);

                // Create output place with unique ID
                let output = ctx.state::<#output_type>(
                    format!("{}__{}", &instance_id, #output_name),
                    format!("{} → {}", #step_display_name, #output_type_str)
                );

                // Wire the transition
                ctx.transition(&instance_id, #step_display_name)
                    #(#auto_input_calls)*
                    .auto_output(#output_name, &output)
                    #(#auto_target_output_calls)*
                    #guard_call
                    .logic(#rhai_script);

                // Return handle for chaining
                output
            }

            /// Step metadata struct generated by `#[step]` macro.
            pub struct #step_name;

            impl aithericon_sdk::StepDefinition for #step_name {
                fn id() -> &'static str {
                    #step_id
                }

                fn name() -> &'static str {
                    #step_display_name
                }

                fn inputs() -> &'static [(&'static str, &'static str)] {
                    &[#((#input_names, #input_types)),*]
                }

                fn output() -> (&'static str, &'static str) {
                    (#output_name, #output_type_str)
                }

                fn script() -> &'static str {
                    #rhai_script
                }
            }
        }
    };

    expanded.into()
}

/// Attribute macro for guards (parsed by #[step], not standalone)
#[proc_macro_attribute]
pub fn guard(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // This is a marker attribute parsed by #[step]
    // Just pass through the item unchanged
    item
}

/// Extract the type name from a Type AST node.
fn extract_type_name(ty: &Type) -> String {
    match ty {
        Type::Path(type_path) => {
            // Get the last segment of the path (e.g., `std::string::String` -> `String`)
            type_path
                .path
                .segments
                .last()
                .map(|seg| seg.ident.to_string())
                .unwrap_or_else(|| "Unknown".to_string())
        }
        Type::Reference(type_ref) => {
            // Handle references by extracting the inner type
            extract_type_name(&type_ref.elem)
        }
        _ => "Unknown".to_string(),
    }
}
