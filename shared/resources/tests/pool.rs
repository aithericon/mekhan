//! Pool-schema tests.
//!
//! Two halves:
//!   1. the resource-kind registry exposes `datacenter` (the one remaining
//!      contended-capacity *kind*) with the correct secret/public field split
//!      (driven by `#[derive(ResourceType)]`). The old `concurrency_limit` /
//!      `runner_group` kinds are GONE — they collapsed into the service-side
//!      `capacity` axes (no kind string survives in `aithericon_resources`).
//!   2. the *separate* pool-schema registry (`aithericon_resources::pool`)
//!      returns the right claim/lease JSON Schemas keyed by dispatch BACKEND
//!      (`schemas_for_backend`), not by kind name.
//!
//! Lives at the `tests/` boundary (not inline) for the same reason as
//! `registry.rs`: it links the `inventory::submit!` sites exactly as a
//! downstream binary would.

use aithericon_resources::lookup;
use aithericon_resources::pool::{schemas_for_backend, PoolBackend};

/// `datacenter` registers with `token` secret and the connection fields public.
#[test]
fn datacenter_kind_registered() {
    let d = lookup("datacenter").expect("datacenter must be registered");

    assert_eq!(d.display_name, "Datacenter");
    assert_eq!(d.icon, "lucide-server");
    assert_eq!(d.oauth_provider, None);
    assert!(!d.dynamic_fields);

    // Discriminated resource: `secret_fields` is the UNION of the per-flavor
    // secrets across the slurm/nomad/http variants (order-robust assertion).
    let secret: Vec<&str> = d.secret_fields.to_vec();
    for s in ["ssh_key", "nomad_token", "token"] {
        assert!(
            secret.contains(&s),
            "datacenter.secret_fields missing `{s}`; got {secret:?}"
        );
    }
    let public: Vec<&str> = d.public_fields.to_vec();
    // The serde tag is listed first, then the union of non-secret variant fields.
    for required in [
        "scheduler_flavor",
        "allocator_url",
        "ssh_host",
        "nomad_addr",
    ] {
        assert!(
            public.contains(&required),
            "datacenter.public_fields missing `{required}`; got {public:?}"
        );
    }
    for s in ["token", "ssh_key", "nomad_token"] {
        assert!(
            !public.contains(&s),
            "datacenter secret `{s}` must NOT be public"
        );
    }
}

/// The deleted legacy kinds must NOT be registered anymore — they collapsed
/// into the service-side `capacity` trait-space. `capacity` is the survivor.
#[test]
fn legacy_pool_kinds_are_gone() {
    assert!(
        lookup("concurrency_limit").is_none(),
        "concurrency_limit kind must be deleted (absorbed into capacity{{seeded}})"
    );
    assert!(
        lookup("runner_group").is_none(),
        "runner_group kind must be deleted (absorbed into capacity{{presence}})"
    );
    assert!(
        lookup("capacity").is_some(),
        "the unified capacity kind must be registered"
    );
}

/// The pool-schema registry produces non-empty claim/lease object schemas for
/// each dispatch backend, keyed by `PoolBackend` (not by kind name).
#[test]
fn schemas_for_each_backend_are_object_schemas() {
    for backend in [PoolBackend::Tokens, PoolBackend::Presence, PoolBackend::Scheduler] {
        let s = schemas_for_backend(backend);
        assert_eq!(
            s.claim.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "{backend:?} claim schema must be an object schema"
        );
        assert_eq!(
            s.lease.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "{backend:?} lease schema must be an object schema"
        );
        let lease_props = s
            .lease
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap_or_else(|| panic!("{backend:?} lease schema missing properties"));
        assert!(
            !lease_props.is_empty(),
            "{backend:?} lease schema must declare at least one field"
        );
    }
}

/// Spot-check the concrete lease fields each backend declares — these are the
/// `<slug>.lease.<field>` borrow surfaces the compiler (R2) wires.
#[test]
fn lease_schemas_declare_expected_fields() {
    // Tokens (seeded) — an opaque admitted-unit identity.
    let tokens_lease = schemas_for_backend(PoolBackend::Tokens).lease;
    let tokens_props = tokens_lease["properties"].as_object().unwrap();
    assert!(tokens_props.contains_key("unit_id"));

    // Presence — runner identity + drain namespace + caps.
    let presence_lease = schemas_for_backend(PoolBackend::Presence).lease;
    let presence_props = presence_lease["properties"].as_object().unwrap();
    for f in ["unit_id", "executor_namespace", "caps"] {
        assert!(
            presence_props.contains_key(f),
            "presence lease missing `{f}`; got {:?}",
            presence_props.keys().collect::<Vec<_>>()
        );
    }

    // Scheduler (datacenter) — the typed universal core + per-flavor union.
    let dc_lease = schemas_for_backend(PoolBackend::Scheduler).lease;
    let dc_props = dc_lease["properties"].as_object().unwrap();
    // Typed core: alloc_id is the only required field; node/expiry/
    // executor_namespace are optional; scheduler is the required per-flavor
    // tagged union. `gpu_uuid` is GONE (no allocator reports it).
    for f in ["alloc_id", "node", "expiry", "executor_namespace", "scheduler"] {
        assert!(
            dc_props.contains_key(f),
            "datacenter lease missing `{f}`; got {:?}",
            dc_props.keys().collect::<Vec<_>>()
        );
    }
    assert!(
        !dc_props.contains_key("gpu_uuid"),
        "datacenter lease must not carry the retired gpu_uuid placeholder"
    );

    // The schema must be SELF-CONTAINED — subschemas inlined, no dangling
    // `definitions`/`$defs` block the engine's SchemaRegistry can't resolve.
    for k in ["definitions", "$defs"] {
        assert!(
            dc_lease.get(k).and_then(|v| v.as_object()).is_none_or(|o| o.is_empty()),
            "lease schema must inline subschemas; found non-empty `{k}`"
        );
    }

    // `scheduler` is the tagged union — inlined as a `oneOf` with a `flavor`
    // discriminator per variant.
    let scheduler = &dc_props["scheduler"];
    assert!(
        scheduler.get("oneOf").and_then(|v| v.as_array()).is_some_and(|a| !a.is_empty()),
        "scheduler must inline a non-empty oneOf; got {scheduler:?}"
    );
}
