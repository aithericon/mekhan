//! Registry smoke tests + the Risk #7 regex assertion from the plan.
//!
//! These tests live at the crate's outer `tests/` boundary (not inline in a
//! `#[cfg(test)]` mod) so the `inventory` linkage exercises the same code
//! path that downstream binaries will: the test binary links every
//! `inventory::submit!` site from `aithericon-resources` exactly as a
//! production binary would, catching the class of bugs where a `submit!`
//! would silently drop on `--release` linkers or when the type is unused.

use aithericon_resources::{lookup, registry::all};

/// Every v1 built-in must appear in the registry.
///
/// Names are spelled out as string literals rather than fed from a `consts`
/// module — these are the wire-stable identifiers and the test should fail
/// loudly if any of them silently drift.
#[test]
fn all_builtin_types_registered() {
    let names: Vec<&str> = all().iter().map(|d| d.name).collect();
    assert!(
        names.contains(&"postgres"),
        "missing `postgres` in registry, got: {names:?}"
    );
    assert!(
        names.contains(&"openai"),
        "missing `openai` in registry, got: {names:?}"
    );
    assert!(
        names.contains(&"slack"),
        "missing `slack` in registry, got: {names:?}"
    );
    assert!(
        names.contains(&"s3"),
        "missing `s3` in registry, got: {names:?}"
    );
    assert!(
        names.contains(&"google_oauth"),
        "missing `google_oauth` in registry, got: {names:?}"
    );

    // Deterministic order — the registry's `all()` contract is "sorted by
    // name". Other call sites depend on this for diff-stable OpenAPI output.
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "registry must return descriptors sorted by name");
}

/// The Postgres descriptor's secret/public partition must match the struct
/// shape exactly. This pins the derive macro's field-walking behavior.
#[test]
fn postgres_descriptor_matches_struct() {
    let pg = lookup("postgres").expect("postgres must be registered");

    // Secret fields: only `password`.
    assert_eq!(
        pg.secret_fields,
        &["password"],
        "Postgres.secret_fields drift detected"
    );

    // Public fields: the non-secret ones, in struct declaration order.
    let public: Vec<&str> = pg.public_fields.to_vec();
    for required in ["host", "port", "database", "username", "sslmode"] {
        assert!(
            public.contains(&required),
            "Postgres.public_fields missing `{required}`; got {public:?}"
        );
    }

    // Sanity: display surface is set up.
    assert_eq!(pg.display_name, "Postgres");
    assert_eq!(pg.icon, "lucide-database");
    assert_eq!(pg.oauth_provider, None);
}

/// Plan Risk #7: assert the existing `extract_secret_keys` regex captures the
/// entire path when fed our `resources/<id>/v<n>#<field>` template. If this
/// breaks, the engine wrap path will silently miss resource secrets and the
/// executor will see a literal template string.
///
/// We test it from outside the engine — `aithericon-secrets` is a dev-dep
/// here precisely so this assertion can be made cheaply now, before the
/// resolver lands.
#[test]
fn regex_captures_resource_path() {
    let input = serde_json::json!({
        "db": {
            "password": "{{secret:resources/abc-123/v3#password}}"
        }
    });

    let keys = aithericon_secrets::extract_secret_keys(&input);
    assert_eq!(
        keys,
        vec!["resources/abc-123/v3#password"],
        "extract_secret_keys must capture the full resource key including `/`, `-`, and `#`"
    );

    // Defense-in-depth: also confirm a multi-key blob with mixed shapes still
    // captures both, so a resource secret next to a legacy KEY doesn't
    // collide on regex anchors.
    let mixed = serde_json::json!({
        "legacy": "{{secret:OPENAI_KEY}}",
        "resource": "{{secret:resources/00000000-0000-0000-0000-000000000001/v1#api_key}}"
    });
    let mut keys = aithericon_secrets::extract_secret_keys(&mixed);
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "OPENAI_KEY".to_string(),
            "resources/00000000-0000-0000-0000-000000000001/v1#api_key".to_string(),
        ]
    );
}
