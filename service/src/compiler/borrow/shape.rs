//! Unified borrow shape: the post-scan record every planner emits.
//!
//! See [`crate::compiler::borrow`] for the surrounding architecture
//! narrative — this module just declares the data shape.

use uuid::Uuid;

use crate::models::template::FieldKind;

/// Rhai block-comment sentinel emitted by `lower_automated_step` /
/// `lower_llm_classify` into the prepare-transition source. The borrow
/// phases splice `job_inputs.push(...)` statements at this marker; any
/// remaining occurrences are stripped at the end of apply_borrows.
pub(crate) const BORROW_MARKER: &str = "/*__BORROWED_INPUTS__*/";

/// One scanned-and-resolved borrow record. The shape is uniform across the
/// five authoring surfaces — what differs per surface is the rewrite
/// strategy carried in [`resolution`](Borrow::resolution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Borrow {
    /// Node whose authored source carries the borrow.
    pub consumer_node_id: String,
    /// Resolved producer node whose parked data the borrow reaches.
    pub producer_node: String,
    /// The author's slug (HumanTask/AutomatedStep `<slug>.<field>` head;
    /// guard's dotted-ref head). Drives staging filenames and is the
    /// key for per-consumer deduplication where applicable.
    pub slug: String,
    /// Per-surface rewrite strategy — what the apply step does with this
    /// borrow once the read-arc is wired.
    pub resolution: BorrowResolution,
}

/// Per-surface rewrite strategy. Read-arc wiring is uniform; what varies
/// is how the consumer's source code reaches the producer's field value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BorrowResolution {
    /// Decision/Loop guard: the consumer's guard / result-mapping source
    /// holds the dotted identifier (`review.invoice_amount`); the apply
    /// step word-boundary-substitutes it for `d_<producer>.<producer_path>`.
    ///
    /// `dotted` is the exact substring the rewriter searches for; e.g.
    /// `"review.invoice_amount"`. `producer_path` is the segment-after-
    /// `d_<producer>.` the rewrite replaces it with; e.g. `"data.invoice_amount"`
    /// (HumanTask producer) or `"detail.outputs.invoice_amount"`
    /// (AutomatedStep producer). The borrow's `slug` is the head of
    /// `dotted`.
    Guard {
        dotted: String,
        producer_path: String,
    },

    /// Python AutomatedStep: stage the producer's whole parked envelope
    /// (with business fields hoisted to the top level) as `<slug>.json`
    /// via a `job_inputs.push(...)` snippet spliced at `BORROW_MARKER`.
    /// The runner's `AccessibleDict` then exposes `<slug>.<field>` to
    /// user Python without any source rewrite. One Borrow per
    /// `(consumer, producer)` pair regardless of how many fields the
    /// Python source reads — the staged file is the whole envelope.
    PythonEnvelope,

    /// HumanTask: the wire-edge transition's Rhai already calls
    /// `__pluck(input, ["<slug>", "<attr>"])` for each placeholder
    /// (emitted by `build_human_task_injection_logic` at lowering).
    /// The apply step substring-rewrites those calls to use
    /// `d_<producer>` instead of `input`. No staging, no marker —
    /// just an in-place needle replacement against the lowered
    /// `__pluck(input, ["<slug>", ` prefix. One Borrow per
    /// `(consumer, producer)` pair (all attr's under the same slug
    /// share the same needle).
    HumanTaskInputRewrite,

    /// Python AutomatedStep with a workspace-level Resource ref. Stages
    /// `<name>.json` from the compiler-spliced `__resources` envelope — there
    /// is no upstream producer to wire a read-arc from, so this variant
    /// intentionally skips `wire_read_arc`. The publish handler resolves the
    /// resource by name to a concrete `(resource_id, latest_version)` pin,
    /// runs the resource resolver to produce the envelope JSON, and splices
    /// `let __resources = #{ ... };` into prepare transitions at publish
    /// time.
    ///
    /// `resource_id` is the rename-safe stable id of the workspace resource;
    /// `latest_version` is the version pinned at publish time. Both ride the
    /// borrow record for downstream consumers (telemetry, `.pyi` generation)
    /// that need the pin without re-querying the workspace.
    ResourceEnvelope {
        /// Workspace-scoped resource name (the `<head>` in Python's
        /// `<head>.<field>` access). Also the staged file stem (`<name>.json`)
        /// and the AccessibleDict Python global.
        name: String,
        /// Pinned resource_id — rename-safe across publishes; deleting the
        /// resource breaks (intentionally).
        resource_id: Uuid,
        /// Resource type name (`postgres`, `openai`, …) — kept on the borrow
        /// for downstream consumers.
        type_name: String,
        /// Resource version pinned at publish time. Carried for replay /
        /// debugging tooling that wants the exact pin without re-querying.
        latest_version: i32,
    },

    /// Node-level Asset binding (docs/20 §5). Stages `<alias>.json` from the
    /// compiler-spliced `__assets` envelope — there is no upstream producer to
    /// wire a read-arc from, so this variant intentionally skips
    /// `wire_read_arc` (symmetric with [`Self::ResourceEnvelope`]). The publish
    /// handler scope-resolves the binding to a concrete `(asset_id, version)`
    /// pin, runs the asset resolver to materialize the records into the
    /// envelope JSON, and splices `let __assets = #{ ... };` into prepare
    /// transitions at publish time.
    ///
    /// Critically, the staged value is the asset's **business data** (its
    /// record rows) — it rides the `job_inputs` staging path, NOT the control
    /// token, honoring the control-data token model (docs/10).
    AssetStaging {
        /// Binding alias — the staged file stem (`<alias>.json`) and the
        /// `__assets` map key the prepare transition indexes.
        alias: String,
        /// Pinned asset id — rename-safe across publishes; deleting the asset
        /// breaks (intentionally).
        asset_id: Uuid,
        /// Asset type id — carried for downstream consumers.
        type_id: Uuid,
        /// Asset version pinned at publish time.
        version: i32,
        /// Names of the asset type's `File`-kind fields. Staged alongside the
        /// records (as `__asset_files.json`) so the runner can deep-wrap each
        /// File field's storage-path value into an `aithericon.File` for lazy
        /// `.retrieve()`. Empty when the asset type has no File fields.
        file_fields: Vec<String>,
    },

    /// Static **named-global** field reference substituted into control-flow
    /// Rhai at apply time (docs/20 §5.1). A static resource public field
    /// (`pg.port`) OR an object-asset record field (`steel.yield_strength`)
    /// referenced from a Decision guard / Loop condition / End or Failure
    /// result mapping is a compile-time constant — the producing global's
    /// pinned `static_vals` never change for this published version. The apply
    /// step boundary-substitutes the literal for `<name>.<ref_path>` in the
    /// consumer node's guard/condition/mapping Rhai (no read-arc, no runtime
    /// envelope). This replaces the former `asset_const` pre-pass +
    /// `inline_object_asset_refs`, and additionally covers static resource
    /// public fields (the convergence dividend).
    ConstantInline {
        /// Named-global reference head (`<name>` in `<name>.<ref_path>`).
        name: String,
        /// Dotted path within the global's `static_vals` (`yield_strength`,
        /// `spec.density`, `port`). The substituted needle is
        /// `<name>.<ref_path>`.
        ref_path: String,
        /// The Rhai literal to substitute, produced by
        /// `json_to_rhai_literal` over the navigated `static_vals` value.
        literal: String,
    },

    /// Map-body item-var envelope. A node that sits inside a Map body and
    /// whose backend Tera-renders its config against staged `<slug>.json`
    /// files (`BorrowShape::Envelope` — ROS, HTTP, SMTP) references the bare
    /// per-element item var (`{{ cand.field }}`). A Python body reads the
    /// same item var as a runner global (the scatter stamps `<item_var>` onto
    /// each body token), but an Envelope backend builds its template context
    /// only from staged files, so the bare item var is invisible there.
    ///
    /// This stages the token-resident element as `<item_var>.json` — sourced
    /// from the in-scope `input.<item_var>` the prepare transition already
    /// binds (the firing body token) — so `{{ item_var.field }}` resolves
    /// identically to the Python body. No upstream producer and no read-arc:
    /// the value rides the firing token, not a parked place (symmetric with
    /// the Resource / Asset envelope variants that also skip `wire_read_arc`).
    MapItemVarEnvelope {
        /// The enclosing Map's `item_var` — the staged file stem
        /// (`<item_var>.json`) and the Tera variable the body config reads.
        item_var: String,
    },

    /// LLM / Kreuzberg AutomatedStep: stage one input file per `(slug, attr)`
    /// via a `job_inputs.push(...)` snippet at `BORROW_MARKER` AND
    /// rewrite the `{{<slug>.<attr>}}` placeholder in the embedded config
    /// to `{{input:NAME}}` (content sites) or `{{input_path:NAME}}` (path
    /// sites). The executor's resolver handles both forms uniformly.
    BackendFieldStage {
        attr: String,
        /// True when this site needs a filesystem path (LLM
        /// `images[].path`, all Kreuzberg sites). False = content site
        /// (LLM prompt / system_prompt / history).
        is_path_site: bool,
        /// Resolved FieldKind of `<attr>` on the producer's data port —
        /// drives Raw vs StoragePath staging dispatch.
        field_kind: FieldKind,
    },
}
