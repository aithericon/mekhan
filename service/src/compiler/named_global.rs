//! Unified **named-global** registry (the convergence target of the three
//! parallel discovery paths — see the approved plan / docs/20 §5).
//!
//! A [`NamedGlobal`] is one entry in a per-template registry, keyed by its
//! identifier name (the `<name>` in a `<name>.<field>` reference): a workspace
//! **resource** or a template-visible **asset**. Today resources, asset
//! bindings, and object-asset field references each re-implement scope
//! resolution, candidate SQL, and pinning; this struct is the single shape
//! that the unified [`crate::process::discover::discover_named_globals`] pass
//! produces and that Phase 2+ consumers (borrow source, `resolve_ref`, the
//! editor picker, `__asset_pins`) read from.
//!
//! Each entry advertises which *channels* it participates in:
//!
//! - **`inline_channel`** — the global has static field values (`static_vals`)
//!   usable as compile-time constants in control-flow Rhai (a resource's
//!   `public_config`, or an object asset's single record). Substituting the
//!   literal needs no read-arc and no runtime envelope.
//! - **`envelope_channel`** — the global needs a runtime envelope/staging
//!   splice into the AIR: a resource's secret envelope (`__resources`), or a
//!   collection asset's bulk staging (`__assets`, `<alias>.json`).
//!
//! This module only defines the data types; it performs **no** I/O. The
//! discovery pass that fills it lives in `service/src/process/discover.rs`.

use std::collections::BTreeMap;

use uuid::Uuid;

use crate::models::asset::Cardinality;
use crate::models::template::PortField;

/// Which kind of named global an entry is. Resources are workspace-scoped
/// (credentials/connection config); assets are template-visible curated record
/// collections (docs/20 §4). Resolution stays typed and distinct per kind —
/// the convergence is at the registry/source level, not a mega apply-fn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // wired by Phase 2+ (borrow source, resolve_ref, picker)
pub enum GlobalKind {
    Resource,
    Asset,
}

/// One named global — a resource or asset referenced as `<name>.<field>` from
/// the template. The `(id, version)` pair is the stable pin baked into the AIR
/// so post-publish edits (a resource rotation / an asset record edit, both of
/// which bump the row's version) never bleed into an already-published
/// workflow.
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields read by Phase 2+ consumers
pub struct NamedGlobal {
    /// The ref-key / reference head: the resource `path` or the asset
    /// `ref_key` the author types as `<name>` in `<name>.<field>`.
    pub name: String,
    /// Resource vs asset.
    pub kind: GlobalKind,
    /// Stable id of the underlying row (resource id / asset id).
    pub id: Uuid,
    /// Version pinned at publish time (resource `latest_version` / asset
    /// `version`).
    pub version: i32,
    /// Resource type name (`postgres`, `openai`, …) for `Resource` globals;
    /// `None` for assets. Carried for the `ResourceEnvelope` borrow record +
    /// downstream `.pyi` / telemetry consumers (matches
    /// [`crate::compiler::resource_refs::KnownResource::type_name`]).
    pub type_name: Option<String>,
    /// Asset type id for `Asset` globals; `None` for resources. Carried for the
    /// `AssetStaging` borrow record + downstream consumers (matches
    /// [`crate::compiler::asset_refs::KnownAsset::type_id`]).
    pub type_id: Option<Uuid>,
    /// Asset cardinality (`object` = single record, `collection` = many rows)
    /// for `Asset` globals; `None` for resources. Drives the staging transport:
    /// an object asset stages its single record as a dict (`<name>.json` ⇒ an
    /// attribute-accessible Python global), a collection stages the row list.
    pub cardinality: Option<Cardinality>,
    /// Typed field contract: the resource type descriptor's public fields, or
    /// the asset type's [`PortField`] schema. Feeds the editor picker /
    /// diagnostics (Phase 3) and the typed `resolve_ref`.
    pub fields: Vec<PortField>,
    /// Static field values usable as compile-time constants: a resource's
    /// `public_config`, or an object-asset's single record (row 0). `None` for
    /// collection assets and for secret-only resources with no public config.
    pub static_vals: Option<serde_json::Value>,
    /// `true` when this global *can* ride a runtime envelope/staging splice
    /// (a structural capability): every resource (secret envelope) and every
    /// asset (bulk staging — a collection as a row list, an object as its single
    /// record dict). Orthogonal to [`Self::inline_channel`]: an object asset
    /// carries both (inline its record into a guard, OR stage it into a Python
    /// body) exactly as a resource with `public_config` carries both.
    pub envelope_channel: bool,
    /// `true` when [`Self::static_vals`] is present, i.e. the global has static
    /// field values inlinable as control-flow constants. Always equals
    /// `static_vals.is_some()`.
    pub inline_channel: bool,
    /// `true` when this graph **actually references** the global through its
    /// envelope/staging channel — a resource named in Python/config or via a
    /// declared alias, or a collection asset bound on a node. Set during
    /// discovery; the publish handler drives the `__resources` / `__assets`
    /// splices off this flag (NOT [`Self::envelope_channel`]), so a global used
    /// *only* as a control-flow constant (`demo_pg.port` in a guard, object
    /// asset field in a Decision) never gets a needless secret/staging splice.
    /// Always `false` in the registry-only (analyze) path, which performs no
    /// splices.
    pub envelope_used: bool,
}

impl NamedGlobal {
    /// Build a resource entry. Resources always carry an envelope channel
    /// (their secrets are spliced as `__resources`); `public_config`, when
    /// non-null, additionally feeds the inline channel.
    #[allow(dead_code)] // ctor used by discover.rs (Phase 1) / borrow source (Phase 2)
    pub(crate) fn from_resource(
        name: String,
        id: Uuid,
        version: i32,
        type_name: String,
        fields: Vec<PortField>,
        public_config: Option<serde_json::Value>,
    ) -> Self {
        // A literal `null` public_config carries no inlinable fields — treat it
        // the same as absent so `inline_channel` stays accurate.
        let static_vals = public_config.filter(|v| !v.is_null());
        let inline_channel = static_vals.is_some();
        Self {
            name,
            kind: GlobalKind::Resource,
            id,
            version,
            type_name: Some(type_name),
            type_id: None,
            cardinality: None,
            fields,
            static_vals,
            envelope_channel: true,
            inline_channel,
            envelope_used: false,
        }
    }

    /// Build an asset entry. `record` is the object-asset's single record (row
    /// 0) for `object` cardinality, or `None` for a `collection` asset.
    ///
    /// Both channels are available and orthogonal: `static_vals` (the object's
    /// record) feeds the **inline** channel (a guard constant), and *every*
    /// asset rides the **envelope/staging** channel — a collection stages its
    /// row list, an object stages its single record dict — so the same
    /// `<name>.<field>` reference resolves in a Python body too. Which channel a
    /// given graph actually *uses* is decided per-reference at discovery
    /// (`envelope_used`), not structurally here.
    #[allow(dead_code)] // ctor used by discover.rs (Phase 1) / borrow source (Phase 2)
    pub(crate) fn from_asset(
        name: String,
        id: Uuid,
        version: i32,
        type_id: Uuid,
        cardinality: Cardinality,
        fields: Vec<PortField>,
        record: Option<serde_json::Value>,
    ) -> Self {
        let static_vals = record.filter(|v| !v.is_null());
        let inline_channel = static_vals.is_some();
        Self {
            name,
            kind: GlobalKind::Asset,
            id,
            version,
            type_name: None,
            type_id: Some(type_id),
            cardinality: Some(cardinality),
            fields,
            static_vals,
            envelope_channel: true,
            inline_channel,
            envelope_used: false,
        }
    }
}

/// Per-template named-global registry, keyed by reference name (`<name>` in
/// `<name>.<field>`). `BTreeMap` so iteration / serialization order is stable —
/// the borrow source and AIR splices emit in this order and stable order keeps
/// the AIR diff-friendly, matching [`crate::compiler::resource_refs::KnownResources`]
/// and [`crate::compiler::asset_refs::KnownAssets`].
pub type KnownGlobals = BTreeMap<String, NamedGlobal>;

/// Lift a legacy [`crate::compiler::resource_refs::KnownResources`] map into a
/// resource-only [`KnownGlobals`] registry. The reverse of
/// [`resources_from_globals`]; used by the internal compile wrappers that still
/// receive a `KnownResources` (no assets / no constant-inline) so they can call
/// the unified `_with_configs` entry that now takes `KnownGlobals`. `fields` is
/// derived from the resource type descriptor's `public_fields`.
#[allow(dead_code)] // consumed by compile.rs wrappers + integration tests
pub fn globals_from_resources(
    resources: &crate::compiler::resource_refs::KnownResources,
) -> KnownGlobals {
    let mut out = KnownGlobals::new();
    for (name, info) in resources {
        let fields: Vec<PortField> = aithericon_resources::registry::lookup(&info.type_name)
            .map(|desc| {
                desc.public_fields
                    .iter()
                    .map(|f| PortField {
                        default: None,
                        name: (*f).to_string(),
                        label: (*f).to_string(),
                        kind: crate::models::template::FieldKind::Json,
                        required: false,
                        options: None,
                        description: None,
                        accept: None,
                        schema: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.insert(
            name.clone(),
            NamedGlobal::from_resource(
                name.clone(),
                info.id,
                info.latest_version,
                info.type_name.clone(),
                fields,
                Some(info.public_config.clone()),
            ),
        );
    }
    out
}

/// Project the `Resource` half of a [`KnownGlobals`] registry back to the
/// legacy [`crate::compiler::resource_refs::KnownResources`] shape, keyed by
/// the resource `path`/`name`. The internal compile pipeline (resource-ref
/// validation, lease-field validation, lowering) still threads `KnownResources`
/// — deriving it here keeps those passes unchanged while the registry becomes
/// the single discovery output.
#[allow(dead_code)] // consumed by compile.rs once the pipeline is rewired
pub(crate) fn resources_from_globals(
    globals: &KnownGlobals,
) -> crate::compiler::resource_refs::KnownResources {
    use crate::compiler::resource_refs::{KnownResource, KnownResources};

    let mut out = KnownResources::new();
    for g in globals.values() {
        if g.kind != GlobalKind::Resource {
            continue;
        }
        out.insert(
            g.name.clone(),
            KnownResource {
                id: g.id,
                type_name: g.type_name.clone().unwrap_or_default(),
                latest_version: g.version,
                public_config: g.static_vals.clone().unwrap_or(serde_json::Value::Null),
            },
        );
    }
    out
}

/// The publish-time `__resources` splice manifest: the `Resource` globals this
/// graph references through their **envelope** channel (Python/config/declared
/// alias), keyed by resource `path`. Excludes resources used *only* as
/// control-flow constants ([`NamedGlobal::envelope_used`] is `false` for those)
/// so no needless secret envelope is baked into the AIR. Matches the set of
/// `ResourceEnvelope` borrows the compiler emits.
pub(crate) fn splice_resources_from_globals(
    globals: &KnownGlobals,
) -> crate::compiler::resource_refs::KnownResources {
    use crate::compiler::resource_refs::{KnownResource, KnownResources};

    let mut out = KnownResources::new();
    for g in globals.values() {
        if g.kind != GlobalKind::Resource || !g.envelope_used {
            continue;
        }
        out.insert(
            g.name.clone(),
            KnownResource {
                id: g.id,
                type_name: g.type_name.clone().unwrap_or_default(),
                latest_version: g.version,
                public_config: g.static_vals.clone().unwrap_or(serde_json::Value::Null),
            },
        );
    }
    out
}

/// The publish-time `__assets` splice manifest: the `Asset` globals this graph
/// references through their **staging** channel — a collection asset bound on a
/// node OR any asset (object or collection) named in a Python/config body —
/// keyed by the registry key (binding alias, else ref-key), which the AIR
/// indexes as `__assets["<key>"]`. Excludes assets referenced *only* as a
/// control-flow constant (those inline via `static_vals`). Matches the set of
/// `AssetStaging` borrows the compiler emits.
pub(crate) fn splice_assets_from_globals(
    globals: &KnownGlobals,
) -> crate::compiler::asset_refs::KnownAssets {
    use crate::compiler::asset_refs::{KnownAsset, KnownAssets};

    let mut out = KnownAssets::new();
    for (key, g) in globals {
        if g.kind != GlobalKind::Asset || !g.envelope_used {
            continue;
        }
        out.insert(
            key.clone(),
            KnownAsset {
                asset_id: g.id,
                type_id: g.type_id.unwrap_or_default(),
                ref_key: g.name.clone(),
                version: g.version,
                cardinality: g.cardinality.unwrap_or(Cardinality::Collection),
            },
        );
    }
    out
}
