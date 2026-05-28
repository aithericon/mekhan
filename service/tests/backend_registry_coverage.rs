//! Backend registry coverage test — replaces the compile-time
//! exhaustiveness check that `ExecutionBackendType` used to give us.
//!
//! Walks `crate::backends::BACKENDS` and asserts:
//!   1. Bijection with `ExecutionBackendType` (every enum variant the
//!      registry claims appears in the registry, no duplicates).
//!   2. Required fields are populated.
//!   3. `executor_wire_name == backend_type.as_wire_str()`.
//!   4. Resource-channel consistency: `StagedFile` backends MUST have
//!      either a static alias path or a ref scanner.
//!   5. Round-trip: `default_editor_config()` returns valid JSON that
//!      `validate` accepts (or returns a clean `CompileError`, never a
//!      panic) for a fresh empty `ValidationCtx`.
//!
//! This test is Phase-1 lean: it walks whatever's in `BACKENDS` and
//! enforces the invariants. As more backends migrate in Phase 2, the
//! coverage grows automatically.

use std::collections::HashMap;

use mekhan_service::backends::{lookup, DispatchMode, ResourceChannel, BACKENDS};

#[test]
fn registry_entries_are_well_formed() {
    assert!(!BACKENDS.is_empty(), "registry must contain at least one backend (Phase 1: SMTP)");

    let mut seen_wires: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut seen_types: std::collections::HashSet<String> = std::collections::HashSet::new();

    for decl in BACKENDS {
        assert!(!decl.meta.display_name.is_empty(), "display_name missing for {:?}", decl.backend_type);
        assert!(!decl.meta.icon.is_empty(), "icon missing for {:?}", decl.backend_type);
        assert_eq!(
            decl.meta.wire_name,
            decl.backend_type.as_wire_str(),
            "executor_wire_name mismatch for {:?}",
            decl.backend_type
        );
        assert!(
            seen_wires.insert(decl.meta.wire_name),
            "duplicate executor_wire_name '{}' in registry",
            decl.meta.wire_name
        );
        let type_key = format!("{:?}", decl.backend_type);
        assert!(
            seen_types.insert(type_key.clone()),
            "duplicate backend_type {} in registry",
            type_key
        );
    }
}

#[test]
fn staged_file_channel_has_a_resource_lookup_path() {
    for decl in BACKENDS {
        if decl.meta.resource_channel == ResourceChannel::StagedFile {
            let has_static_path = !decl.resource_alias_paths.is_empty();
            let has_scanner = decl.ref_scanner.is_some();
            assert!(
                has_static_path || has_scanner,
                "{:?} declares ResourceChannel::StagedFile but has no resource_alias_paths AND no ref_scanner — \
                 there's no way to discover which workspace resource to stage",
                decl.backend_type
            );
        }
    }
}

#[test]
fn lookup_round_trips_for_every_decl() {
    for decl in BACKENDS {
        let looked_up = lookup(decl.backend_type).expect("lookup must find registered decl");
        assert_eq!(
            looked_up.meta.wire_name, decl.meta.wire_name,
            "lookup returned the wrong decl"
        );
    }
}

#[test]
fn default_editor_config_round_trips_through_validate() {
    // `default_editor_config()` is the JSON the editor seeds when a step is
    // first set to this backend. It MUST either pass validate cleanly or
    // produce a deterministic `CompileError::Validation` (e.g. SMTP's
    // default has no recipients yet, which is intentional). Either way:
    // no panics, no infinite loops, no `Compilation` errors (those would
    // indicate serializer breakage).
    use mekhan_service::backends::ValidationCtx;
    use mekhan_service::compiler::CompileError;

    let node_files = HashMap::new();
    let ctx = ValidationCtx { node_id: "round-trip", node_files: &node_files };

    for decl in BACKENDS {
        let cfg = (decl.default_editor_config)();
        match (decl.validate)(&cfg, &ctx) {
            Ok(_) => { /* fully valid default — fine */ }
            Err(CompileError::Validation(_)) | Err(CompileError::BackendPlaceholderSyntax { .. }) => {
                // Acceptable: the default may be intentionally incomplete
                // (SMTP's default has empty recipient list, etc.).
            }
            Err(other) => {
                panic!(
                    "{:?}: default_editor_config produced a non-Validation error: {:?}",
                    decl.backend_type, other
                );
            }
        }
    }
}

/// Engine-effect backends (e.g. CatalogueQuery → `catalogue_lookup`) are
/// inherently inline — they execute as a single Petri builtin-effect
/// transition fired by the engine itself, with no executor job to dispatch
/// to a scheduler-net. The decl MUST therefore declare
/// `schedulable: false`; otherwise the editor would expose a Scheduled
/// deployment toggle the compiler has no path to honour.
///
/// Phase 2.e introduces the first `EngineEffect` decl (CatalogueQuery);
/// this invariant locks in the contract for every future engine-effect
/// backend that lands in the registry.
#[test]
fn engine_effect_decls_are_non_schedulable() {
    for decl in BACKENDS {
        if let DispatchMode::EngineEffect { handler } = decl.meta.dispatch_mode {
            assert!(
                !decl.meta.schedulable,
                "{:?} declares DispatchMode::EngineEffect {{ handler: \"{handler}\" }} \
                 but also schedulable: true — engine effects don't dispatch executor \
                 jobs and have no scheduler-net path, so the Scheduled toggle would \
                 be unsupported at compile time",
                decl.backend_type
            );
        }
    }
}

#[test]
fn descriptor_serialization_matches_decl() {
    use mekhan_service::backends::descriptors;
    let all = descriptors();
    assert_eq!(all.len(), BACKENDS.len(), "descriptor count must match registry length");
    for (decl, descriptor) in BACKENDS.iter().zip(all.iter()) {
        assert_eq!(descriptor.name, decl.meta.wire_name);
        assert_eq!(descriptor.display_name, decl.meta.display_name);
        assert_eq!(descriptor.icon, decl.meta.icon);
        assert_eq!(descriptor.schedulable, decl.meta.schedulable);
        assert_eq!(descriptor.consumes_declared_outputs, decl.consumes_declared_outputs);
        assert_eq!(descriptor.output_authoring, decl.output_authoring);
        assert_eq!(
            descriptor.default_output_port.fields.len(),
            decl.default_output_fields.len(),
            "default_output_fields length mismatch for {:?}",
            decl.backend_type
        );
    }
}

#[test]
fn derived_authoring_backends_have_deriver() {
    use mekhan_service::backends::OutputAuthoring;
    for decl in BACKENDS {
        if matches!(decl.output_authoring, OutputAuthoring::Derived) {
            assert!(
                decl.derive_output_port.is_some(),
                "{:?} declares output_authoring=Derived but is missing derive_output_port — \
                 the frontend will hit a 500 on POST /api/v1/backends/{}/derive-output",
                decl.backend_type,
                decl.meta.wire_name,
            );
        }
    }
}
