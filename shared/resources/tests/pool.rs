//! R1 pool-kind tests.
//!
//! Two halves:
//!   1. the resource-kind registry exposes `token_pool` + `datacenter` with the
//!      correct secret/public field split (driven by `#[derive(ResourceType)]`);
//!   2. the *separate* pool-kind registry (`aithericon_resources::pool`) returns
//!      a descriptor for each, with the right backend and non-empty claim/lease
//!      JSON Schemas.
//!
//! Lives at the `tests/` boundary (not inline) for the same reason as
//! `registry.rs`: it links the `inventory::submit!` sites exactly as a
//! downstream binary would.

use aithericon_resources::lookup;
use aithericon_resources::pool::{pool_kind, PoolBackend};

/// `token_pool` registers as a no-secret kind with `capacity` + `unit_label`
/// public.
#[test]
fn token_pool_kind_registered() {
    let d = lookup("token_pool").expect("token_pool must be registered");

    assert_eq!(d.display_name, "Token Pool");
    assert_eq!(d.icon, "lucide-layers");
    assert_eq!(d.oauth_provider, None);
    assert!(!d.dynamic_fields);

    assert!(
        d.secret_fields.is_empty(),
        "token_pool is platform-owned — no secret; got {:?}",
        d.secret_fields
    );
    let public: Vec<&str> = d.public_fields.to_vec();
    for required in ["capacity", "unit_label"] {
        assert!(
            public.contains(&required),
            "token_pool.public_fields missing `{required}`; got {public:?}"
        );
    }
}

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

/// The pool-kind registry maps each kind's wire name to the right backend and
/// produces non-empty claim/lease schemas. Non-pool kinds return `None`.
#[test]
fn pool_kind_registry_backends_and_schemas() {
    let tokens = pool_kind("token_pool").expect("token_pool pool-kind must exist");
    assert_eq!(tokens.backend, PoolBackend::Tokens);
    assert_eq!(tokens.kind_name, "token_pool");

    let sched = pool_kind("datacenter").expect("datacenter pool-kind must exist");
    assert_eq!(sched.backend, PoolBackend::Scheduler);
    assert_eq!(sched.kind_name, "datacenter");

    // Non-pool kinds are not claimable.
    assert!(pool_kind("postgres").is_none());
    assert!(pool_kind("smtp").is_none());

    for d in [tokens, sched] {
        let claim = (d.claim_schema)();
        let lease = (d.lease_schema)();
        // Both are object schemas with a non-empty property set.
        assert_eq!(
            claim.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "{} claim_schema must be an object schema",
            d.kind_name
        );
        assert_eq!(
            lease.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "{} lease_schema must be an object schema",
            d.kind_name
        );
        let lease_props = lease
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap_or_else(|| panic!("{} lease_schema missing properties", d.kind_name));
        assert!(
            !lease_props.is_empty(),
            "{} lease_schema must declare at least one field",
            d.kind_name
        );
    }
}

/// Spot-check the concrete lease fields each kind declares — these are the
/// `<slug>.lease.<field>` borrow surfaces R2 will wire.
#[test]
fn lease_schemas_declare_expected_fields() {
    let tokens_lease = (pool_kind("token_pool").unwrap().lease_schema)();
    let tokens_props = tokens_lease["properties"].as_object().unwrap();
    assert!(tokens_props.contains_key("unit_id"));

    let dc_lease = (pool_kind("datacenter").unwrap().lease_schema)();
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
