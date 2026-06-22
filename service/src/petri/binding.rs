//! Launch-time resource/pool **binding resolution** (Phase C).
//!
//! Phase B auto-derives a [`RequirementsManifest`] at compile: one
//! [`RequirementSlot`] per distinct resource/pool reference the template binds,
//! keyed by its alias, typed by the resolved `resource_type`, and tagged with
//! the AIR addresses the home-workspace baseline baked (`pool-{resource_id}`
//! net ids + `__resources` splice keys). The persisted `air_json` is a concrete
//! baseline for the template's HOME workspace — every slot is already bound to
//! whatever the publishing workspace's name-match resolved.
//!
//! This module turns those slots into RUN-TIME parameters. At launch we resolve
//! an EFFECTIVE binding for each slot through a precedence chain (high → low):
//!
//! 1. **Per-instance override** — `CreateInstanceRequest.bindings[slot_key]`,
//!    the caller's explicit `slot_key -> resource_id` for this one run.
//! 2. **Per-workspace default** — a `template_resource_bindings` row for
//!    `(chain_root_id, workspace_id, slot_key)` (set via
//!    `PUT /templates/{id}/bindings`).
//! 3. **Platform auto-bind** — exactly ONE `scope_kind = 'platform'` resource
//!    whose `resource_type` matches the slot. Ambiguous (>1) → left unbound.
//! 4. **Home-workspace name-match baseline** — the binding the persisted
//!    baseline AIR already baked (the legacy alias resolution). A slot with no
//!    higher-tier override is "satisfied by baseline" and the launcher emits NO
//!    substitution for it (byte-identical to today).
//! 5. Otherwise **unbound** — if `required`, the run-gate rejects the launch.
//!
//! The resolver does NOT mutate AIR or deploy nets — it returns a typed
//! [`ResolvedBindings`] the launcher consumes. A slot's effective resource is
//! only carried here when it DIFFERS from the baked baseline (tiers 1–3); a
//! tier-4 baseline slot is recorded as satisfied-by-baseline with no resource,
//! so the launcher leaves its AIR addresses untouched.

use std::collections::HashMap;

use uuid::Uuid;

use crate::compiler::{RequirementSlot, RequirementsManifest};
use crate::models::resource::ResourceRow;
use crate::models::template::WorkflowTemplate;
use crate::AppState;

/// Which precedence tier satisfied a slot. Surfaced on the readiness endpoint
/// so the binding UI can show WHY a slot is bound (and let the operator decide
/// whether to override).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BindingTier {
    /// Tier 1 — the caller's per-instance `bindings` map.
    InstanceOverride,
    /// Tier 2 — a per-workspace default `template_resource_bindings` row.
    WorkspaceDefault,
    /// Tier 3 — a single matching `scope_kind = 'platform'` resource.
    PlatformAutoBind,
    /// Tier 4 — the home-workspace name-match the persisted baseline AIR baked.
    /// The launcher leaves this slot's AIR addresses untouched.
    HomeBaseline,
}

/// A slot resolved to a concrete substitute resource (tiers 1–3). Carries
/// everything the launcher needs to re-splice + rewrite the AIR for this slot.
#[derive(Debug, Clone)]
pub struct BoundSlot {
    /// Slot key == binding alias == `__resources` index == the resource id
    /// inside the baked `pool-{id}` net id.
    pub slot_key: String,
    /// The effective resource id this slot resolved to.
    pub resource_id: Uuid,
    /// The effective resource version (the pin's version, or the resource's
    /// `latest_version` when unpinned).
    pub version: i32,
    /// The resource's `resource_type` (validated == `slot.resource_type`).
    pub resource_type: String,
    /// `true` when the bound resource is `scope_kind = 'platform'`. The
    /// launcher re-deploys this resource's pool net under the TENANT workspace
    /// (not `PLATFORM_SCOPE_ID`) so the engine's intra-workspace bridge gate +
    /// NATS subject routing line up (see the launcher's platform-rebind note).
    pub is_platform: bool,
    /// Which tier resolved this binding (audit / readiness display).
    pub tier: BindingTier,
}

/// The outcome of resolving every slot in a template's manifest for one launch.
#[derive(Debug, Clone, Default)]
pub struct ResolvedBindings {
    /// Slots whose effective resource DIFFERS from the baked baseline (tiers
    /// 1–3). The launcher re-splices `__resources` + rewrites `pool-{old}` →
    /// `pool-{new}` for each. Keyed by slot_key.
    pub substitutions: HashMap<String, BoundSlot>,
    /// `required` slots that resolved by NO tier (not even the baseline). The
    /// launcher run-gate rejects the launch when this is non-empty.
    pub unbound_required: Vec<RequirementSlot>,
    /// Resource ids of HOME-BASELINE (tier-4) pool slots — the `pool-<id>`
    /// capacity nets the instance bridges to that the launcher leaves baked
    /// (no substitution). The launcher re-`ensure`s each under the launching
    /// workspace before deploy so a workspace-owned pool the engine hibernated,
    /// drifted, or lost is (re)materialized rather than failing the instance's
    /// activation gate with `BRIDGE_TARGET_NET_MISSING`. Substituted (tier-1–3)
    /// pools are ensured by the substitution loop instead.
    pub baseline_pools: Vec<Uuid>,
}

impl ResolvedBindings {
    /// `true` when the launch may proceed (no required slot is unbound).
    pub fn is_launchable(&self) -> bool {
        self.unbound_required.is_empty()
    }
}

/// All failure modes of [`resolve_effective_bindings`]. The launcher maps these
/// to caller-facing 400/422; DB faults surface as 500.
#[derive(Debug, thiserror::Error)]
pub enum BindingError {
    /// An offered resource (override / default) does not exist, is
    /// soft-deleted, or is not visible to the launching workspace (neither the
    /// tenant nor the platform tier).
    #[error("requirement slot '{slot_key}': resource {resource_id} not found or not visible")]
    ResourceNotFound { slot_key: String, resource_id: Uuid },

    /// An offered resource's `resource_type` does not match the slot's declared
    /// type — mirrors [`crate::compiler::CompileError::SlotTypeMismatch`] at
    /// launch time. A `postgres` slot can't be bound to an `openai` resource.
    #[error(
        "requirement slot '{slot_key}': expects resource type '{expected}' but \
         resource {resource_id} is type '{found}'"
    )]
    SlotTypeMismatch {
        slot_key: String,
        resource_id: Uuid,
        expected: String,
        found: String,
    },

    #[error("binding resolution DB error: {0}")]
    Db(String),
}

/// Resolve an EFFECTIVE binding for every slot in `template.requirements_json`.
///
/// `overrides` is the caller's per-instance `slot_key -> resource_id` map (tier
/// 1). `workspace_id` is the LAUNCHING workspace (the template's workspace for a
/// user launch). `principal` is the launching user (audit / future ACL).
///
/// Returns [`ResolvedBindings`]: the tier-1–3 substitutions the launcher must
/// apply, plus any required slots that resolved by no tier. A template with a
/// NULL/empty manifest yields an empty result (the launcher then takes the
/// byte-identical legacy path).
pub async fn resolve_effective_bindings(
    state: &AppState,
    template: &WorkflowTemplate,
    workspace_id: Uuid,
    _principal: Uuid,
    overrides: &HashMap<String, Uuid>,
) -> Result<ResolvedBindings, BindingError> {
    let manifest = match parse_manifest(template) {
        Some(m) if !m.is_empty() => m,
        // NULL / empty manifest → nothing to resolve; legacy fast-path.
        _ => return Ok(ResolvedBindings::default()),
    };

    let chain_root_id = template.chain_root_id();
    let mut out = ResolvedBindings::default();

    // The baked baseline AIR resolves resources against the workspace it was
    // PUBLISHED in. That's the template's own workspace for a normally-published
    // template, but the SOURCE workspace for a fork (whose `air_json` +
    // `requirements_json` were copied verbatim and still bake the source's
    // resource ids/secrets). Tier 4 (baseline) may therefore only satisfy a slot
    // when the LAUNCHING workspace IS that baseline-origin workspace — otherwise
    // the baked resources are foreign (workspace-scoped to another tenant) and
    // "trusting the baseline" would silently run on the origin tenant's
    // credentials. In a non-home workspace a slot must resolve via tier 1–3 or
    // it is gated.
    let is_home = workspace_id == baseline_origin_workspace(template);

    // Tier 2 lookup: per-workspace defaults for this chain root, in one query.
    let defaults = load_workspace_defaults(state, chain_root_id, workspace_id).await?;

    for slot in &manifest.slots {
        // Tier 1: per-instance override.
        if let Some(&resource_id) = overrides.get(&slot.key) {
            let bound = resolve_and_check(
                state,
                slot,
                resource_id,
                None,
                workspace_id,
                BindingTier::InstanceOverride,
            )
            .await?;
            out.substitutions.insert(slot.key.clone(), bound);
            continue;
        }

        // Tier 2: per-workspace default.
        if let Some((resource_id, version)) = defaults.get(&slot.key).copied() {
            let bound = resolve_and_check(
                state,
                slot,
                resource_id,
                version,
                workspace_id,
                BindingTier::WorkspaceDefault,
            )
            .await?;
            out.substitutions.insert(slot.key.clone(), bound);
            continue;
        }

        // Tier 3: platform auto-bind — exactly one platform resource of the
        // slot's type. Ambiguous (>1) leaves the slot to fall through (the
        // operator must choose explicitly via override/default).
        if let Some(bound) = platform_auto_bind(state, slot).await? {
            out.substitutions.insert(slot.key.clone(), bound);
            continue;
        }

        // Tier 4: home-workspace name-match baseline. The persisted baseline
        // AIR already baked a concrete resource for this slot's alias IF the
        // home workspace had a matching resource at publish — that's exactly
        // what `air_addresses[slot.key].net_ids` / `resource_keys` being
        // non-empty records. A slot the baseline actually baked is
        // satisfied-by-baseline (no substitution emitted); the launcher leaves
        // its AIR untouched. Only valid in the baseline's ORIGIN workspace — a
        // fork in another workspace must rebind (tier 1–3) or be gated.
        if is_home && baseline_satisfies(&manifest, slot) {
            // Record this slot's baked pool net(s) so the launcher re-ensures
            // them under the launching workspace (self-heal for a hibernated /
            // drifted / engine-lost workspace-owned pool). DataResource slots
            // bake `resource_keys` but no `net_ids` → contribute nothing here.
            out.baseline_pools
                .extend(baseline_pool_resource_ids(&manifest, slot));
            continue;
        }

        // Tier 5: unbound. Required slots gate the launch.
        if slot.required {
            out.unbound_required.push(slot.clone());
        }
    }

    Ok(out)
}

/// Deserialize the manifest off the template row. A decode failure is treated
/// as "no manifest" (the launcher then legacy-launches) rather than a hard
/// error — a corrupt sidecar must never strand an otherwise-launchable run.
fn parse_manifest(template: &WorkflowTemplate) -> Option<RequirementsManifest> {
    let raw = template.requirements_json.as_ref()?;
    match serde_json::from_value::<RequirementsManifest>(raw.clone()) {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(
                template_id = %template.id,
                error = %e,
                "requirements_json failed to decode; launching without binding-aware substitution"
            );
            None
        }
    }
}

/// Did the persisted baseline AIR bake a concrete address for this slot? A slot
/// with a non-empty `net_ids` (pool slot) or `resource_keys` (DataResource)
/// address was bound by the home-workspace name-match at publish. An empty
/// address means the baseline baked nothing — there is nothing to fall back on,
/// so the slot is genuinely unbound (and gated if required).
fn baseline_satisfies(manifest: &RequirementsManifest, slot: &RequirementSlot) -> bool {
    manifest
        .air_addresses
        .get(&slot.key)
        .map(|a| !a.net_ids.is_empty() || !a.resource_keys.is_empty())
        .unwrap_or(false)
}

/// The resource ids of the `pool-<resource_id>` capacity nets a baseline slot
/// baked. Parsed back out of the slot's baked `net_ids` (the same
/// `pool_net_id(resource_id)` scheme the compiler emits), so the launcher can
/// re-ensure each net under the launching workspace. A DataResource slot (no
/// `net_ids`) yields nothing; an unparseable id is skipped (defensive — a
/// future net-id scheme change must not strand the launch).
fn baseline_pool_resource_ids(
    manifest: &RequirementsManifest,
    slot: &RequirementSlot,
) -> Vec<Uuid> {
    let Some(addresses) = manifest.air_addresses.get(&slot.key) else {
        return Vec::new();
    };
    addresses
        .net_ids
        .iter()
        .filter_map(|net_id| net_id.strip_prefix("pool-"))
        .filter_map(|raw| Uuid::parse_str(raw).ok())
        .collect()
}

/// The workspace the template's baked baseline AIR resolved its resources in —
/// i.e. where the persisted `air_json` / `requirements_json.air_addresses` are
/// valid. For a fork this is the SOURCE workspace recorded in `forked_from`
/// (the `air_json` was copied verbatim and still bakes the source's resource
/// ids + secret paths); for a normally-published template it is the template's
/// own workspace. Used to gate tier-4 baseline satisfaction to the home
/// workspace so a fork can't silently launch on the origin tenant's resources.
fn baseline_origin_workspace(template: &WorkflowTemplate) -> Uuid {
    template
        .forked_from
        .as_ref()
        .and_then(|v| v.get("workspace_id"))
        .and_then(|w| w.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or(template.workspace_id)
}

/// Per-workspace defaults for one chain root, as `slot_key -> (resource_id,
/// Option<version>)`.
async fn load_workspace_defaults(
    state: &AppState,
    chain_root_id: Uuid,
    workspace_id: Uuid,
) -> Result<HashMap<String, (Uuid, Option<i32>)>, BindingError> {
    let rows: Vec<(String, Uuid, Option<i32>)> = sqlx::query_as(
        "SELECT slot_key, resource_id, resource_version \
         FROM template_resource_bindings \
         WHERE chain_root_id = $1 AND workspace_id = $2",
    )
    .bind(chain_root_id)
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| BindingError::Db(e.to_string()))?;

    Ok(rows
        .into_iter()
        .map(|(k, id, ver)| (k, (id, ver)))
        .collect())
}

/// Load a resource visible to `workspace_id` (or the platform tier), enforce
/// the slot's type, and build a [`BoundSlot`]. `version` pins the resource
/// version; `None` uses the resource's `latest_version`.
async fn resolve_and_check(
    state: &AppState,
    slot: &RequirementSlot,
    resource_id: Uuid,
    version: Option<i32>,
    workspace_id: Uuid,
    tier: BindingTier,
) -> Result<BoundSlot, BindingError> {
    let resource = load_visible_resource(state, resource_id, workspace_id).await?;
    let Some(resource) = resource else {
        return Err(BindingError::ResourceNotFound {
            slot_key: slot.key.clone(),
            resource_id,
        });
    };
    type_check(slot, resource_id, &resource.resource_type)?;
    Ok(BoundSlot {
        slot_key: slot.key.clone(),
        resource_id,
        version: version.unwrap_or(resource.latest_version),
        resource_type: resource.resource_type,
        is_platform: resource.scope_kind == "platform",
        tier,
    })
}

/// Tier 3: bind a slot to the SOLE `scope_kind = 'platform'` resource of the
/// slot's type, if exactly one exists. `Ok(None)` when zero or many match
/// (ambiguous platform tier is left for an explicit override/default).
async fn platform_auto_bind(
    state: &AppState,
    slot: &RequirementSlot,
) -> Result<Option<BoundSlot>, BindingError> {
    // A DataResource slot with no resource_type (analyze-path fallback) can't
    // be platform-matched — skip it.
    if slot.resource_type.is_empty() {
        return Ok(None);
    }
    let rows: Vec<(Uuid, i32)> = sqlx::query_as(
        "SELECT id, latest_version FROM resources \
         WHERE scope_kind = 'platform' AND resource_type = $1 AND deleted_at IS NULL \
         LIMIT 2",
    )
    .bind(&slot.resource_type)
    .fetch_all(&state.db)
    .await
    .map_err(|e| BindingError::Db(e.to_string()))?;

    // Exactly one match → auto-bind. Zero or ambiguous (≥2) → leave unbound.
    let [(resource_id, latest_version)] = rows.as_slice() else {
        return Ok(None);
    };
    Ok(Some(BoundSlot {
        slot_key: slot.key.clone(),
        resource_id: *resource_id,
        version: *latest_version,
        resource_type: slot.resource_type.clone(),
        is_platform: true,
        tier: BindingTier::PlatformAutoBind,
    }))
}

/// Load a resource by id that is visible to `workspace_id` — the tenant's own
/// rows OR the globally-visible platform tier — and not soft-deleted. Mirrors
/// the `resource_resolver`'s visibility gate.
async fn load_visible_resource(
    state: &AppState,
    resource_id: Uuid,
    workspace_id: Uuid,
) -> Result<Option<ResourceRow>, BindingError> {
    sqlx::query_as::<_, ResourceRow>(
        "SELECT id, workspace_id, path, resource_type, display_name, \
                latest_version, deleted_at, created_by, created_at, updated_at, \
                updated_by, scope_kind, scope_id, display_path, restricted \
         FROM resources \
         WHERE id = $1 AND (workspace_id = $2 OR scope_kind = 'platform') \
           AND deleted_at IS NULL",
    )
    .bind(resource_id)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| BindingError::Db(e.to_string()))
}

/// Enforce the slot type contract: the offered resource's type must equal the
/// slot's declared `resource_type`. An empty slot type (analyze-path fallback)
/// accepts any type defensively.
fn type_check(
    slot: &RequirementSlot,
    resource_id: Uuid,
    found: &str,
) -> Result<(), BindingError> {
    if !slot.resource_type.is_empty() && slot.resource_type != found {
        return Err(BindingError::SlotTypeMismatch {
            slot_key: slot.key.clone(),
            resource_id,
            expected: slot.resource_type.clone(),
            found: found.to_string(),
        });
    }
    Ok(())
}

/// Readiness of ONE slot for a given workspace — the projection the
/// `GET /templates/{id}/requirements` endpoint returns per slot.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct SlotReadiness {
    /// The requirement slot.
    pub slot: RequirementSlot,
    /// `true` when the slot resolves by some tier (override/default/platform/
    /// baseline) for the current workspace.
    pub satisfied: bool,
    /// The tier that satisfies it (when `satisfied`), else `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<BindingTier>,
    /// The effective resource id (tiers 1–3) when known. `None` for a
    /// baseline-satisfied slot (its resource is baked in the AIR, not surfaced
    /// here) or an unbound slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<Uuid>,
}

/// The full per-workspace readiness of a template's manifest: one
/// [`SlotReadiness`] per slot plus the aggregate launch gate. This is the SINGLE
/// implementation shared by the `GET /templates/{id}/requirements` /
/// `PUT /templates/{id}/bindings` endpoints, by the fork path-match remap, and
/// (transitively, via [`resolve_effective_bindings`]) by the launcher's run-gate
/// — so "is this slot satisfied?" can never drift across surfaces.
#[derive(Debug, Clone, Default)]
pub struct ManifestReadiness {
    /// One readiness entry per slot in manifest order.
    pub slots: Vec<SlotReadiness>,
    /// `true` when every REQUIRED slot is satisfied for the workspace — a launch
    /// would pass the run-gate.
    pub launchable: bool,
}

impl ManifestReadiness {
    /// Number of slots that still need an explicit binding — i.e. required slots
    /// that no tier (override/default/platform/baseline) satisfies. This is the
    /// "N slots need configuration" count the binding UI renders.
    pub fn needs_configuration(&self) -> usize {
        self.slots.iter().filter(|r| !r.satisfied).count()
    }
}

/// Compute per-workspace readiness for a template's requirement manifest.
///
/// Resolves the effective binding for every slot through the precedence chain
/// (with NO per-instance overrides — readiness is the standing, pre-launch view)
/// and projects it into one [`SlotReadiness`] per slot. A slot is `satisfied`
/// when it is a tier-1–3 substitution OR the home-workspace baseline AIR baked a
/// concrete address for it; a REQUIRED slot that no tier resolves is unsatisfied
/// and gates the launch.
///
/// `workspace_id` is the workspace the readiness is computed FOR (the caller's
/// active workspace on the requirements endpoint; the FORK TARGET workspace when
/// deciding which default bindings to seed). A template with a NULL/empty
/// manifest yields an empty, launchable readiness.
pub async fn compute_readiness(
    state: &AppState,
    template: &WorkflowTemplate,
    workspace_id: Uuid,
    principal: Uuid,
) -> Result<ManifestReadiness, BindingError> {
    let manifest = parse_manifest(template).unwrap_or_default();
    if manifest.is_empty() {
        return Ok(ManifestReadiness {
            slots: Vec::new(),
            launchable: true,
        });
    }

    let resolved =
        resolve_effective_bindings(state, template, workspace_id, principal, &HashMap::new())
            .await?;

    let unbound: std::collections::HashSet<&str> = resolved
        .unbound_required
        .iter()
        .map(|s| s.key.as_str())
        .collect();

    let slots = manifest
        .slots
        .iter()
        .map(|slot| {
            if let Some(bound) = resolved.substitutions.get(&slot.key) {
                SlotReadiness {
                    slot: slot.clone(),
                    satisfied: true,
                    tier: Some(bound.tier),
                    resource_id: Some(bound.resource_id),
                }
            } else if unbound.contains(slot.key.as_str()) {
                SlotReadiness {
                    slot: slot.clone(),
                    satisfied: false,
                    tier: None,
                    resource_id: None,
                }
            } else {
                // Not a substitution and not unbound-required ⇒ satisfied by the
                // home-workspace baseline AIR (or an optional slot left unbound).
                let satisfied = slot.required;
                SlotReadiness {
                    slot: slot.clone(),
                    satisfied,
                    tier: satisfied.then_some(BindingTier::HomeBaseline),
                    resource_id: None,
                }
            }
        })
        .collect();

    Ok(ManifestReadiness {
        slots,
        launchable: resolved.is_launchable(),
    })
}

#[cfg(test)]
mod tests {
    //! Pure-logic tests for the binding resolver's DB-independent parts:
    //! manifest parsing, the baseline-satisfaction gate, the type-check gate,
    //! and the launchability / needs-configuration aggregates. The full
    //! precedence chain (`resolve_effective_bindings`) needs an `AppState` + a
    //! live Postgres and is exercised in `service/tests/resource_bindings_e2e.rs`.

    use super::*;
    use crate::compiler::requirements::{SlotAirAddresses, SlotRole};
    use std::collections::BTreeMap;

    fn slot(key: &str, ty: &str, required: bool) -> RequirementSlot {
        RequirementSlot {
            key: key.to_string(),
            resource_type: ty.to_string(),
            role: SlotRole::ExecutorCapacity,
            required,
            request_shape: None,
            used_by: vec!["n1".to_string()],
        }
    }

    fn manifest_with(
        slots: Vec<RequirementSlot>,
        addrs: Vec<(&str, SlotAirAddresses)>,
    ) -> RequirementsManifest {
        let mut air_addresses = BTreeMap::new();
        for (k, a) in addrs {
            air_addresses.insert(k.to_string(), a);
        }
        RequirementsManifest {
            slots,
            air_addresses,
        }
    }

    #[test]
    fn baseline_satisfies_when_pool_net_id_baked() {
        let m = manifest_with(
            vec![slot("prod_gpu", "capacity", true)],
            vec![(
                "prod_gpu",
                SlotAirAddresses {
                    net_ids: vec!["pool-abc".to_string()],
                    resource_keys: vec![],
                },
            )],
        );
        assert!(baseline_satisfies(&m, &m.slots[0]));
    }

    #[test]
    fn baseline_satisfies_when_resource_key_baked() {
        let m = manifest_with(
            vec![slot("main_db", "postgres", true)],
            vec![(
                "main_db",
                SlotAirAddresses {
                    net_ids: vec![],
                    resource_keys: vec!["main_db".to_string()],
                },
            )],
        );
        assert!(baseline_satisfies(&m, &m.slots[0]));
    }

    #[test]
    fn baseline_does_not_satisfy_when_no_address_baked() {
        // Empty address (or no entry) means the home-workspace name-match baked
        // nothing — there is nothing to fall back on, so the slot is unbound.
        let m = manifest_with(
            vec![slot("missing", "capacity", true)],
            vec![(
                "missing",
                SlotAirAddresses {
                    net_ids: vec![],
                    resource_keys: vec![],
                },
            )],
        );
        assert!(!baseline_satisfies(&m, &m.slots[0]));

        // Slot with no address entry at all → also unsatisfied.
        let m2 = manifest_with(vec![slot("orphan", "capacity", true)], vec![]);
        assert!(!baseline_satisfies(&m2, &m2.slots[0]));
    }

    #[test]
    fn baseline_pool_resource_ids_parses_pool_net_ids() {
        let rid = Uuid::new_v4();
        let m = manifest_with(
            vec![slot("prod_gpu", "capacity", true)],
            vec![(
                "prod_gpu",
                SlotAirAddresses {
                    net_ids: vec![format!("pool-{rid}")],
                    resource_keys: vec![],
                },
            )],
        );
        assert_eq!(
            baseline_pool_resource_ids(&m, &m.slots[0]),
            vec![rid],
            "the launcher must recover the pool resource id to re-ensure its net"
        );
    }

    #[test]
    fn baseline_pool_resource_ids_skips_data_resources_and_garbage() {
        // A DataResource slot bakes resource_keys but no net_ids → no pools.
        let data = manifest_with(
            vec![slot("main_db", "postgres", true)],
            vec![(
                "main_db",
                SlotAirAddresses {
                    net_ids: vec![],
                    resource_keys: vec!["main_db".to_string()],
                },
            )],
        );
        assert!(baseline_pool_resource_ids(&data, &data.slots[0]).is_empty());

        // A net id that isn't `pool-<uuid>` is skipped, not panicked on.
        let garbage = manifest_with(
            vec![slot("weird", "capacity", true)],
            vec![(
                "weird",
                SlotAirAddresses {
                    net_ids: vec!["pool-not-a-uuid".to_string(), "staging-xyz".to_string()],
                    resource_keys: vec![],
                },
            )],
        );
        assert!(baseline_pool_resource_ids(&garbage, &garbage.slots[0]).is_empty());
    }

    #[test]
    fn type_check_passes_on_match_and_rejects_mismatch() {
        let id = Uuid::new_v4();
        let s = slot("main_db", "postgres", true);
        assert!(type_check(&s, id, "postgres").is_ok());

        let err = type_check(&s, id, "openai").expect_err("type mismatch must error");
        match err {
            BindingError::SlotTypeMismatch {
                expected, found, ..
            } => {
                assert_eq!(expected, "postgres");
                assert_eq!(found, "openai");
            }
            other => panic!("expected SlotTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn type_check_empty_slot_type_accepts_any() {
        // Analyze-path fallback: an empty slot type accepts any resource type.
        let s = slot("x", "", true);
        assert!(type_check(&s, Uuid::new_v4(), "anything").is_ok());
    }

    #[test]
    fn resolved_bindings_launchable_iff_no_unbound_required() {
        let mut r = ResolvedBindings::default();
        assert!(r.is_launchable(), "empty resolution is launchable");
        r.unbound_required.push(slot("prod_gpu", "capacity", true));
        assert!(!r.is_launchable(), "an unbound required slot blocks launch");
    }

    #[test]
    fn manifest_is_empty_predicate() {
        assert!(RequirementsManifest::default().is_empty());
        let m = manifest_with(vec![slot("k", "capacity", true)], vec![]);
        assert!(!m.is_empty());
    }

    #[test]
    fn manifest_readiness_needs_configuration_counts_unsatisfied() {
        let mut mr = ManifestReadiness::default();
        mr.slots.push(SlotReadiness {
            slot: slot("a", "capacity", true),
            satisfied: true,
            tier: Some(BindingTier::HomeBaseline),
            resource_id: None,
        });
        mr.slots.push(SlotReadiness {
            slot: slot("b", "capacity", true),
            satisfied: false,
            tier: None,
            resource_id: None,
        });
        mr.slots.push(SlotReadiness {
            slot: slot("c", "postgres", true),
            satisfied: false,
            tier: None,
            resource_id: None,
        });
        assert_eq!(mr.needs_configuration(), 2);
    }

    #[test]
    fn parse_manifest_round_trips_and_tolerates_corruption() {
        let manifest = manifest_with(
            vec![slot("prod_gpu", "capacity", true)],
            vec![(
                "prod_gpu",
                SlotAirAddresses {
                    net_ids: vec!["pool-abc".to_string()],
                    resource_keys: vec![],
                },
            )],
        );

        let mut tmpl = sample_template();
        tmpl.requirements_json = Some(serde_json::to_value(&manifest).unwrap());
        let parsed = parse_manifest(&tmpl).expect("valid manifest decodes");
        assert_eq!(parsed.slots.len(), 1);
        assert_eq!(parsed.slots[0].key, "prod_gpu");

        // NULL → None (legacy fast-path).
        tmpl.requirements_json = None;
        assert!(parse_manifest(&tmpl).is_none());

        // Corrupt sidecar → None (must never hard-fail an otherwise-launchable run).
        tmpl.requirements_json = Some(serde_json::json!({ "slots": "not-an-array" }));
        assert!(parse_manifest(&tmpl).is_none());
    }

    /// A minimal `WorkflowTemplate` for the manifest-parse test (only `id` for
    /// the warn log + `requirements_json` matter to `parse_manifest`).
    fn sample_template() -> WorkflowTemplate {
        use chrono::Utc;
        WorkflowTemplate {
            id: Uuid::new_v4(),
            name: "t".into(),
            description: String::new(),
            base_template_id: None,
            parent_id: None,
            version: 1,
            is_latest: true,
            published: true,
            published_at: None,
            published_by: None,
            graph: serde_json::json!({}),
            air_json: None,
            interface_json: None,
            requirements_json: None,
            source_ref: None,
            author_id: Uuid::nil(),
            updated_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            workspace_id: Uuid::nil(),
            visibility: "workspace".into(),
            owner_template_id: None,
            template_kind: "workflow".into(),
            origin: None,
            coordinate: None,
            presentation: None,
            lifecycle_status: "active".into(),
            superseded_by: None,
            forked_from: None,
            my_effective_role: None,
        }
    }
}
