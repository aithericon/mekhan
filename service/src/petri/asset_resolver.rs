//! `AssetResolver` — the asset analog of [`super::resource_resolver`]
//! (docs/20 §5/§6).
//!
//! Turns the compiler's publish-pinned [`KnownAssets`] manifest (alias ->
//! `(asset_id, version)`) into a JSON envelope the publish handler splices into
//! the AIR:
//!
//! ```text
//! { "<alias>": [ {record row}, {record row}, ... ], ... }
//! ```
//!
//! Each alias maps to the asset's **whole record collection** at the pinned
//! version — materialized from `asset_records` ordered by `row_idx`. The
//! records are the asset's *business data*; they ride `job_inputs` staging
//! (spliced `__assets` map → `job_inputs.push(... __assets["alias"] ...)`),
//! NEVER the control token, honoring the control-data token model (docs/10).
//!
//! **What this module does NOT do** (mirrors the resource resolver's contract):
//! - Talk to Vault / handle secrets — assets have none (docs/20 §1).
//! - Mutate AIR. The publish handler does that via [`splice_assets_into_air`].
//!
//! Records are immutable-per-version, so resolving the SAME pinned
//! `(asset_id, version)` always yields the SAME envelope — post-publish record
//! edits bump the asset version and never bleed into an already-published
//! workflow (the AIR pinned the version).

use serde_json::{Map as JsonMap, Value as JsonValue};
use sqlx::PgPool;
use thiserror::Error;

use crate::compiler::asset_refs::KnownAssets;

/// Failure modes surfaced by [`AssetResolver::resolve_known`]. Wraps
/// `sqlx::Error` directly — the caller's HTTP layer maps DB failures to 500s
/// uniformly, like the resource resolver.
#[derive(Debug, Error)]
pub enum AssetResolverError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Stateless service handle. Cheap to clone (Postgres pools are `Arc`-shaped)
/// and `Send + Sync` — same shape as [`super::resource_resolver::ResourceResolver`].
#[derive(Clone)]
pub struct AssetResolver {
    db: PgPool,
}

impl AssetResolver {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Borrow the pool — useful for tests that seed rows on the same DB the
    /// resolver reads.
    pub fn pool(&self) -> &PgPool {
        &self.db
    }

    /// Materialize every pinned asset's records into the envelope JSON ready
    /// for splicing into the AIR. Keyed by the staged-file stem the prepare
    /// transition indexes (binding alias, else ref-key).
    ///
    /// The value shape follows the asset's **cardinality**: a `Collection`
    /// stages the JSON array of record rows (ordered by `row_idx`); an `Object`
    /// stages its single record **dict** (row 0, `{}` if empty) so that, once
    /// staged as `<key>.json`, the Python runner exposes it as an
    /// attribute-accessible global (`steel_spec.yield_strength`) — symmetric
    /// with how a resource's public fields are reached.
    ///
    /// Unlike the resource resolver this writes no audit rows (assets carry no
    /// secret access to attribute) and has no ACL gate (visibility was already
    /// enforced at scope-resolution time by the publish handler).
    pub async fn resolve_known(
        &self,
        known: &KnownAssets,
    ) -> Result<JsonValue, AssetResolverError> {
        use crate::models::asset::Cardinality;

        let mut envelope = JsonMap::with_capacity(known.len());
        for (alias, info) in known {
            let rows: Vec<(serde_json::Value,)> = sqlx::query_as(
                "SELECT data FROM asset_records \
                 WHERE asset_id = $1 AND version = $2 \
                 ORDER BY row_idx ASC",
            )
            .bind(info.asset_id)
            .bind(info.version)
            .fetch_all(&self.db)
            .await?;
            let mut records: Vec<JsonValue> = rows.into_iter().map(|(d,)| d).collect();
            let value = match info.cardinality {
                Cardinality::Object => records
                    .drain(..)
                    .next()
                    .unwrap_or_else(|| JsonValue::Object(JsonMap::new())),
                Cardinality::Collection => JsonValue::Array(records),
            };
            envelope.insert(alias.clone(), value);
        }
        Ok(JsonValue::Object(envelope))
    }
}

/// Splice `let __assets = #{ ... };` at the top of every prepare transition
/// whose Rhai logic references any of the binding aliases. Mirrors
/// [`super::resource_resolver::splice_resources_into_air`] exactly: one
/// declaration per transition carrying **all** referenced aliases, idempotent
/// against a repeat call, only spliced into transitions that actually index
/// `__assets["<alias>"]`.
///
/// Called at publish time (not launch) so the AIR persisted in
/// `workflow_template_versions.air_json` already carries the materialized
/// records. The values are pure data (no secrets / no `null`-keyword hazard
/// from the secret-template path) — the resource resolver's null-dropping
/// rules still apply to any `null` field *inside* a record row, so we reuse
/// the same `json_to_rhai_literal` from there.
pub fn splice_assets_into_air(
    mut air: JsonValue,
    envelope: &JsonValue,
    aliases: &[&str],
) -> JsonValue {
    let rhai_decl = build_assets_decl(envelope, aliases);
    if rhai_decl.is_empty() {
        return air;
    }

    let Some(transitions) = air.get_mut("transitions").and_then(|t| t.as_array_mut()) else {
        return air;
    };

    for t in transitions {
        let Some(t_obj) = t.as_object_mut() else {
            continue;
        };

        // Splice targets: the backend-job `prepare`/`acquire` transitions (the
        // historical asset-staging sites) PLUS a Map's `t_<id>_scatter` (feature
        // B: a bare-`itemsRef` asset binding rewrites `let __src = __assets["a"]`
        // into the scatter — a pure-Rhai transition, NOT a prepare suffix, so it
        // would otherwise never receive the `let __assets = #{...}` declaration).
        // The `references_any` check below still gates the actual splice, so this
        // only widens the candidate set to transitions that genuinely index the
        // envelope.
        let is_splice_target = t_obj
            .get("id")
            .and_then(JsonValue::as_str)
            .map(|id| {
                crate::compiler::borrow::apply::has_prepare_transition_suffix(id)
                    || id.ends_with("_scatter")
            })
            .unwrap_or(false);
        if !is_splice_target {
            continue;
        }

        let Some(logic) = t_obj.get_mut("logic") else {
            continue;
        };
        let Some(logic_obj) = logic.as_object_mut() else {
            continue;
        };
        let Some(source) = logic_obj.get("source").and_then(JsonValue::as_str) else {
            continue;
        };
        let source = source.to_owned();

        // Only splice into transitions whose logic actually references an alias.
        let references_any = aliases.iter().any(|a| {
            source.contains(&format!("__assets[\"{a}\"]"))
                || source.contains(&format!("__assets['{a}']"))
        });
        if !references_any {
            continue;
        }

        // Idempotent guard.
        if source.contains("let __assets") {
            continue;
        }

        let new_source = format!("{rhai_decl}\n{source}", source = source);
        logic_obj.insert("source".to_string(), JsonValue::String(new_source));
    }

    air
}

/// Build `let __assets = #{ "alias": [ #{...}, ... ], ... };` from the
/// resolver's JSON envelope. Each alias's value is the record-array literal.
/// Reuses the resource resolver's `json_to_rhai_literal` so `null` fields
/// inside a record row are dropped (objects) / unit-`()`d (arrays) — Rhai has
/// no `null` keyword.
fn build_assets_decl(envelope: &JsonValue, aliases: &[&str]) -> String {
    let JsonValue::Object(top) = envelope else {
        return String::new();
    };
    let mut entries: Vec<String> = Vec::with_capacity(aliases.len());
    for alias in aliases {
        let Some(records) = top.get(*alias) else {
            continue;
        };
        entries.push(format!(
            "\"{alias}\": {records}",
            alias = escape_rhai_key(alias),
            records = super::resource_resolver::json_to_rhai_literal(records),
        ));
    }
    if entries.is_empty() {
        return String::new();
    }
    format!("let __assets = #{{ {} }};", entries.join(", "))
}

fn escape_rhai_key(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_assets_decl_basic() {
        let env = json!({
            "steel": [
                { "name": "S235", "yield_strength": 235 },
                { "name": "S355", "yield_strength": 355 }
            ]
        });
        let decl = build_assets_decl(&env, &["steel"]);
        assert!(decl.starts_with("let __assets = #{ "));
        assert!(decl.contains("\"steel\": ["));
        assert!(decl.contains("\"name\": \"S235\""));
        assert!(decl.contains("\"yield_strength\": 235"));
        assert!(decl.ends_with(" };"));
    }

    #[test]
    fn build_assets_decl_empty_envelope_is_empty() {
        let env = json!({});
        assert_eq!(build_assets_decl(&env, &["steel"]), "");
    }

    /// `null` fields inside a record row must NOT leak the literal `null` into
    /// the Rhai source (no `null` keyword in Rhai) — dropped for objects.
    #[test]
    fn build_assets_decl_drops_null_record_fields() {
        let env = json!({ "mats": [ { "name": "x", "note": null } ] });
        let decl = build_assets_decl(&env, &["mats"]);
        assert!(!decl.contains("null"), "no `null` literal may escape: {decl}");
        assert!(decl.contains("\"name\": \"x\""));
        assert!(!decl.contains("\"note\""));
    }

    #[test]
    fn splice_skips_non_prepare() {
        let air = json!({
            "transitions": [
                {
                    "id": "t_x_consume",
                    "logic": { "type": "Rhai", "source": "__assets[\"steel\"]" }
                }
            ]
        });
        let env = json!({ "steel": [ { "name": "S235" } ] });
        let out = splice_assets_into_air(air, &env, &["steel"]);
        let src = out["transitions"][0]["logic"]["source"].as_str().unwrap();
        assert!(!src.contains("let __assets"));
    }

    #[test]
    fn splice_inserts_once_per_prepare() {
        let air = json!({
            "transitions": [
                {
                    "id": "t_step_prepare",
                    "logic": {
                        "type": "Rhai",
                        "source": "job_inputs.push(#{ \"name\": \"steel.json\", \"source\": #{ \"type\": \"inline\", \"value\": __assets[\"steel\"] } });"
                    }
                }
            ]
        });
        let env = json!({ "steel": [ { "name": "S235" } ] });
        let out = splice_assets_into_air(air, &env, &["steel"]);
        let src = out["transitions"][0]["logic"]["source"].as_str().unwrap();
        assert!(src.contains("let __assets = #{"));
        assert!(src.contains("\"name\": \"S235\""));
        // Idempotent.
        let env2 = json!({ "steel": [ { "name": "S235" } ] });
        let out2 = splice_assets_into_air(out, &env2, &["steel"]);
        let src2 = out2["transitions"][0]["logic"]["source"].as_str().unwrap();
        assert_eq!(src2.matches("let __assets").count(), 1);
    }
}
