//! Integration tests for the #[step] macro.
//!
//! These tests verify that the macro correctly generates:
//! - StepDefinition trait implementations
//! - Proper Rhai script from Rust function bodies
//! - Functional step functions that return PlaceHandle<T>

use aithericon_sdk::prelude::*;

// Define test token types
#[token]
struct Input {
    id: i64,
    value: String,
}

#[token]
struct Output {
    input_id: i64,
    result: String,
}

#[token]
struct Combined {
    a_id: i64,
    b_id: i64,
    sum: i64,
}

#[token]
struct Final {
    combined_sum: i64,
}

// Test basic step macro
#[step("transform", "Transform Input")]
fn transform(input: Input) -> Output {
    Output {
        input_id: input.id,
        result: input.value,
    }
}

// Test step with multiple inputs
#[step("combine", "Combine Two Inputs")]
fn combine(a: Input, b: Input) -> Combined {
    Combined {
        a_id: a.id,
        b_id: b.id,
        sum: a.id + b.id,
    }
}

// Test step with arithmetic
#[step("compute", "Compute Value")]
fn compute(x: Input) -> Output {
    Output {
        input_id: x.id * 2,
        result: x.value,
    }
}

// Test step for chaining
#[step("finalize", "Finalize Combined")]
fn finalize(combined: Combined) -> Final {
    Final {
        combined_sum: combined.sum,
    }
}

#[test]
fn test_step_metadata_single_input() {
    assert_eq!(TransformStep::id(), "transform");
    assert_eq!(TransformStep::name(), "Transform Input");
    assert_eq!(TransformStep::inputs(), &[("input", "Input")]);
    assert_eq!(TransformStep::output(), ("output", "Output"));
}

#[test]
fn test_step_metadata_multiple_inputs() {
    assert_eq!(CombineStep::id(), "combine");
    assert_eq!(CombineStep::name(), "Combine Two Inputs");
    assert_eq!(CombineStep::inputs(), &[("a", "Input"), ("b", "Input")]);
    assert_eq!(CombineStep::output(), ("combined", "Combined"));
}

#[test]
fn test_generated_rhai_script_single_input() {
    let script = TransformStep::script();
    // Should contain the output map with field assignments
    assert!(script.contains("#{ output:"));
    assert!(script.contains("input_id: input.id"));
    assert!(script.contains("result: input.value"));
}

#[test]
fn test_generated_rhai_script_multiple_inputs() {
    let script = CombineStep::script();
    assert!(script.contains("#{ combined:"));
    assert!(script.contains("a_id: a.id"));
    assert!(script.contains("b_id: b.id"));
    assert!(script.contains("sum: a.id + b.id"));
}

#[test]
fn test_generated_rhai_script_arithmetic() {
    let script = ComputeStep::script();
    assert!(script.contains("input_id: x.id * 2"));
}

#[test]
fn test_functional_step_creates_transition() {
    let mut ctx = Context::new("test");

    let inputs = ctx.state::<Input>("inputs", "Inputs");

    // Use functional pattern - returns PlaceHandle
    let outputs = transform(&mut ctx, &inputs);

    let scenario = ctx.build();

    // Verify output place was created with correct ID pattern (now includes counter)
    assert_eq!(outputs.id(), "transform_1__output");

    // Verify place exists in scenario
    let output_place = scenario
        .places
        .iter()
        .find(|p| p.id == "transform_1__output");
    assert!(output_place.is_some());
    let output_place = output_place.unwrap();
    assert_eq!(output_place.name, "Transform Input → Output");

    // Verify transition was created (ID now includes counter)
    assert_eq!(scenario.transitions.len(), 1);
    let transition = &scenario.transitions[0];
    assert_eq!(transition.id, "transform_1");
    assert_eq!(transition.name, "Transform Input");

    // Verify input port
    assert_eq!(transition.input_ports.len(), 1);
    assert_eq!(transition.input_ports[0].name, "input");

    // Verify output port
    assert_eq!(transition.output_ports.len(), 1);
    assert_eq!(transition.output_ports[0].name, "output");

    // Verify arcs
    assert_eq!(transition.inputs.len(), 1);
    assert_eq!(transition.inputs[0].place, "inputs");

    assert_eq!(transition.outputs.len(), 1);
    assert_eq!(transition.outputs[0].place, "transform_1__output");

    // Verify logic contains the generated script
    if let TransitionLogic::Rhai { source } = &transition.logic {
        assert!(source.contains("output:"));
        assert!(source.contains("input_id: input.id"));
    } else {
        panic!("Expected Rhai logic");
    }
}

#[test]
fn test_functional_step_multiple_inputs() {
    let mut ctx = Context::new("test");

    let input_a = ctx.state::<Input>("input_a", "Input A");
    let input_b = ctx.state::<Input>("input_b", "Input B");

    // Use functional pattern
    let combined = combine(&mut ctx, &input_a, &input_b);

    let scenario = ctx.build();

    // Verify output place ID pattern (now includes counter)
    assert_eq!(combined.id(), "combine_1__combined");

    let transition = &scenario.transitions[0];
    assert_eq!(transition.input_ports.len(), 2);
    assert_eq!(transition.inputs.len(), 2);
    assert_eq!(transition.inputs[0].place, "input_a");
    assert_eq!(transition.inputs[1].place, "input_b");
    assert_eq!(transition.outputs[0].place, "combine_1__combined");
}

#[test]
fn test_functional_step_chaining() {
    let mut ctx = Context::new("test");

    let input_a = ctx.state::<Input>("input_a", "Input A");
    let input_b = ctx.state::<Input>("input_b", "Input B");

    // Chain steps together - output of combine feeds into finalize
    let combined = combine(&mut ctx, &input_a, &input_b);
    let final_result = finalize(&mut ctx, &combined);

    let scenario = ctx.build();

    // Verify both places were created (now includes counter)
    assert_eq!(combined.id(), "combine_1__combined");
    assert_eq!(final_result.id(), "finalize_1__final");

    // Verify both transitions were created
    assert_eq!(scenario.transitions.len(), 2);

    // Find transitions by ID (now includes counter)
    let combine_trans = scenario
        .transitions
        .iter()
        .find(|t| t.id == "combine_1")
        .unwrap();
    let finalize_trans = scenario
        .transitions
        .iter()
        .find(|t| t.id == "finalize_1")
        .unwrap();

    // Verify combine outputs to combine_1__combined
    assert_eq!(combine_trans.outputs[0].place, "combine_1__combined");

    // Verify finalize inputs from combine_1__combined
    assert_eq!(finalize_trans.inputs[0].place, "combine_1__combined");
}

#[test]
fn test_wire_terminal() {
    let mut ctx = Context::new("test");

    let inputs = ctx.state::<Input>("inputs", "Inputs");
    let outputs = transform(&mut ctx, &inputs);

    // Wire to terminal
    ctx.wire_terminal(&outputs, "completed");

    let scenario = ctx.build();

    // Verify terminal place was created
    let terminal = scenario
        .places
        .iter()
        .find(|p| p.id == "completed_terminal");
    assert!(terminal.is_some());
    let terminal = terminal.unwrap();
    assert_eq!(terminal.name, "completed (Terminal)");
    assert_eq!(terminal.place_type, "terminal");

    // Verify pass-through transition was created
    let pass_through = scenario
        .transitions
        .iter()
        .find(|t| t.id == "completed_to_terminal");
    assert!(pass_through.is_some());
    let pass_through = pass_through.unwrap();
    assert_eq!(pass_through.inputs[0].place, "transform_1__output");
    assert_eq!(pass_through.outputs[0].place, "completed_terminal");
}

#[test]
fn test_step_definition_trait_is_implemented() {
    // This test verifies that the generated struct implements StepDefinition
    fn assert_step_definition<T: StepDefinition>() {}

    assert_step_definition::<TransformStep>();
    assert_step_definition::<CombineStep>();
    assert_step_definition::<ComputeStep>();
    assert_step_definition::<FinalizeStep>();
}
