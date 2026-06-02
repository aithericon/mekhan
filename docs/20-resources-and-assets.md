# 20 — Resources & Assets: scoping, folders, and a curated asset layer

> Status: **design + initial implementation** (branch `feat/assets-layer`).
> Supersedes the flat, workspace-only resource model with (a) hierarchical
> scoping + virtual folders for resources, and (b) a brand-new **asset** layer:
> user-typed, curated, *static* content (material parameters, simulation
> scripts, reference artifacts) stored as schema-validated JSONB rows + S3 files,
> consumed by workflow nodes as ordinary staged inputs.

This document is the contract for the feature. It resolves a long design
dialogue (see commit history / grill notes). Read it before touching
`shared/resources/`, `service/src/handlers/resources.rs`, the compiler resource
path, or the frontend resource components.

---

## 1. Motivation & the three-layer split

Resources today are **flat, secret-focused objects**: a small `(workspace_id,
path)`-unique credential bundle with a public/secret split, Vault-backed, spliced
into compiled AIR as `{{secret:…}}` templates at instance launch. That is the
*wrong* shape for "a material database" or "a simulation script" — those are
*content*, not credentials. They have no secret to split and are not spliced into
AIR; they are data a node reads.

So we keep three **cleanly separated** layers:

| Layer | What it is | Lifecycle | Backing |
|---|---|---|---|
| **Resource** | Credential / connection primitive (**unchanged** in nature) | mutable, versioned, pinned | `public_config` + Vault secrets |
| **Asset** *(new)* | User-typed, curated **static** content | mutable, versioned, pinned | Postgres JSONB rows + S3 for file fields |
| **Catalog** (`catalogue_entries`) | Machine-produced job-output ledger (**unchanged**) | immutable, execution-keyed | S3 artifacts + provenance |

Key decisions and their rationale:

- **Resource stays the credential primitive.** Its usefulness comes from a
  Rust-side backend integration (Postgres knows how to open a connection, SMTP how
  to send). A user-defined *credential kind* with a schema but no backend is
  **inert** — no node could consume it. Therefore **resource kinds remain a
  closed, backend-wired set**. The "custom types with a schema" desire is
  satisfied entirely by the **asset** layer.
- **Asset is a new, separate table** — not an extension of `catalogue_entries`.
  The catalog is an *immutable, execution-keyed ledger of produced outputs*;
  assets are *mutable, versioned, human-authored, scoped, user-typed*. The
  lifecycles are opposite. A catalog entry can be *promoted into* an asset (see
  §4, file-field dual-source).
- **Assets are static.** Live/external data (a real materials DB) is **out of
  scope** here; it arrives later as an **import/sync job** that lands rows into an
  asset. Runtime query / behavioral adapters (a `Queryable` capability, leased
  adapters) are likewise **deferred** (see §9). This document deliberately builds
  only the static-content path, leaving clean additive seams for those.

---

## 2. Scoping (applies to resources, assets, and asset-types — uniform)

Today: owner is a bare `workspace_id`, `(workspace_id, path)` unique. The platform
hierarchy is **workspace → projects (an M:N grouping of templates, *not* a tree)
→ templates(=workflows) → instances**.

**New model — polymorphic owner + downward visibility + most-specific-wins:**

- Owner becomes `(scope_kind, scope_id)` where `scope_kind ∈ {workspace, project,
  template}`. A resource/asset/asset-type is owned by **exactly one** scope.
- **Visibility flows downward.** A binding inside template `T` can *see* anything
  owned by `T`, by any project that contains `T`, or by the workspace.
- **Resolution is most-specific-wins:** `template` shadows `project` shadows
  `workspace`. ("Define the prod DB once at workspace, override per-project, keep
  a throwaway private to one workflow.")
- **Ambiguity is a hard error.** If `T` belongs to two projects that *both* define
  the same ref-key, the scopes are **incomparable** → **`CompileError`**
  (SlugConflict-style), never a silent pick. This matches the platform's
  "compiler is the borrow-checker; ambiguity is an error, not a guess" ethos.

Migration: existing rows map trivially to `scope_kind='workspace', scope_id =
<old workspace_id>`. The `(workspace_id, path)` uniqueness generalizes to
`(scope_kind, scope_id, ref_key)` unique.

> **Governance:** *defining* a type or creating a workspace/project-scoped
> resource/asset is gated to **editor/admin** via existing workspace member roles;
> creating *instances* of an existing type is the everyday **editor** capability.

---

## 3. Virtual folders (organization within a scope)

`path` today does **double duty**: it is both the organizational name *and* the
Python/binding reference key (`prod_pg.host`), constrained to a flat identifier
`^[a-z][a-z0-9_]*$` — no slashes. Folders want slashes. Those roles fight, so we
**decouple** them:

- The **ref-key stays flat and identifier-safe** (`prod_pg`). The borrow-checker,
  resolver, and `<slug>.<field>` references are **completely untouched**.
- Add a **separate `display_path` string** (e.g. `databases/production`) used
  purely for UI grouping. Folders are *emergent* from the prefix — **no folders
  table**. Rename/move = edit the string.

This is an additive, free column. If per-folder ACL / first-class movable folders
are needed later, the virtual prefix seeds real folder rows — a clean upgrade.
**Never couple organization to the ref-key (option C was rejected).**

---

## 4. Asset data model

### 4.1 Asset types (user-defined schema)

An **asset type** is a user-defined schema: an ordered list of fields, each typed
by the **existing unified `FieldKind`** vocabulary
(`service/src/models/template.rs`, 9 variants: `Text, Textarea, Number, Bool,
Select, File, Signature, Timestamp, Json`). **Reuse `PortField` / `Port::json_schema`
/ `FieldKind::json_schema` wholesale** — do *not* invent an asset-specific field
language. Validation of records uses the same JSON-Schema + token-validation path
already used for ports.

- Nested objects/arrays are reachable only via the `PortField.schema` JSON-Schema
  escape hatch. Flat scalar columns are the table-builder / CSV-friendly core.
- **Records are self-contained:** there is **no `resource_ref` and no `asset_ref`
  field kind** in v1. An asset record is pure validated data, so the
  borrow-checker never looks *inside* an asset for *composition*; binding an asset
  is opaque "inject + stage". (Composition refs are a deferred additive field
  kind — §9.) *Exception:* a **single-record** asset's fields are referenceable as
  constant-valued `<ref_key>.<field>` refs (§5.1) — that lifts the opacity for
  reads (not writes), since the field value is a compile-time constant.

#### File fields are dual-source

A `File` field's value may come from **either**:
1. a fresh **upload** (→ a new S3 object), **or**
2. a **pick from the existing data catalog** (a `catalogue_entry` → reuse its
   `storage_path`).

Both resolve to the **same thing**: an S3 storage path
(`InputSource::StoragePath`). This is *not* a reference field kind in the
`resource_ref` sense — it is a file *source selector*, and it delivers the
"promote catalog entry → asset" capability at field granularity. The
borrow-checker still never enters the asset.

### 4.2 Assets (typed collection of records)

An **asset** is a named, **version-pinned**, scope-owned **collection of records**
of one asset type. An "object" asset is just the **1-row degenerate case** (the
builder renders a single-row form instead of a grid). Records are stored as
**schema-validated JSONB rows** in one generic store — **no per-type DDL**.

- File fields store an S3 pointer (or catalogue-entry-derived path) *inside* the
  row JSONB.
- Populated via: **object/table builder** (grid; 1-row form for object types),
  **CSV importer** (flat scalar columns), and **file upload** (for `File` fields).
- Versioned and immutable-per-version like resources: editing rows creates a new
  version; running instances **pin** the version they launched against.

### 4.3 Schema evolution — additive / loosening only

When a type's schema changes and rows already exist:

- **Allowed without migration:** add an **optional** field; **widen** a field.
  Existing JSONB rows stay valid as-is (a missing new field reads as
  absent/null).
- **Disallowed in v1:** rename, remove, retype, or newly-**require** a field — any
  breaking change. Enforce this server-side on type update. A breaking change is a
  deliberate act: clone to a new type (a future opt-in migration path is §9).

No migration engine is built. This matches the platform's "schemas are contracts,
breaking changes are loud" posture.

---

## 5. Consumption — assets are sugar over staging

A node **binds an asset** with a node-level authoring selection (analogous to
`resource_alias`). At launch the binding is **version-pinned** exactly like
`resource_pins` (see §6). At compile time the binding **lowers to an
`InputDeclaration`**: the asset's records (the whole collection) are materialized
into a single staged **`{alias}.json`** input.

**File fields travel as storage-path strings *inside* that record JSON.** A
`File` value (whether upload-sourced or catalog-sourced) is the S3 `storage_path`
of the object; it is carried verbatim in the row data, and the consuming node
fetches it on demand via that path. This is uniform across `object` and
`collection` cardinalities — a 5000-row table where every row has a file must
*not* eagerly pre-stage 5000 objects, so the storage-path-in-data model is the
correct general shape (and mirrors how catalogue files are referenced). Eagerly
**pre-staging** an individual `File` field to a known local path — convenient for
the 1-row "simulation script" case so the node receives the script already on
disk — is a deferred additive enhancement (see §9), not v1.

The consuming node reads the staged input as an **ordinary input**. Critically,
**business data never enters the control token** — this honors the control-data
token model (`docs/10`): only slim control tokens move; the asset's rows are
parked/staged, not inlined into config. The entire consumption path is *"an asset
compiles down to an input staging,"* riding the mature staging machinery.

**Granularity:** v1 binds the **whole collection** (the node does its own lookup
in code). Author-picked-row (pick "the `steel` row" at authoring time) is a thin
additive sugar deferred to §9. Runtime filter/query is deferred (§9).

### 5.1 Field-level references (constant-valued) — assets as first-class refs

Staging hands a step the *whole* collection as `<alias>.json` for in-code
processing. That is **second-class** next to producer `<slug>.<field>` references,
which work in the variable picker, guards, Decision/Loop conditions, End
`resultMapping`, and config fields. Assets earn **reference parity** for the case
where it is well-defined: a **single typed record**.

**Why it's clean.** An asset type's `fields_json` *is* a `Port` — the same typed
field contract a producer emits — and the pinned records are **static at compile
time** (materialized in the AIR `__assets` envelope at publish). So an asset field
reference is a **compile-time constant**: it needs neither a runtime read-arc nor
`__assets` in the guard's Rhai scope.

**Shape by cardinality:**
- **Object asset (or a G2 picked row)** → `<ref_key>.<field>` — a single typed
  record, identical to a producer output. *The primary, shipped form.*
- **Collection asset** → `<ref_key>[*].<field>` (projection over rows, reusing the
  existing Map `[*]` grammar) + `<ref_key>` (the whole array). *Deferred second
  pass.*

**Scope: template-global, like resources.** Producer refs are upstream-reachable;
resource names and asset ref-keys are **globally visible identifiers** unioned
into the reference scope (`merged_identifier_scope`). So `steel.yield_strength` is
referenceable from *any* node — a Decision guard, an End mapping — not just a step
that "consumes" it, exactly like a resource. Only **scope-visible** assets (§2,
most-specific-wins) are referenceable.

**Resolution = compile-time substitution.** A new borrow planner scans guards /
conditions / mappings / config for `<ref_key>.<field>` where `ref_key` is a
scope-visible object asset, pins `(asset_id, version)` (into `asset_pins`, so
lineage §9 still works), reads the field off the pinned record, and substitutes
the Rhai literal in place via `replace_word_boundary` (the same rewrite read-arcs
use) — `steel.yield_strength > 250` becomes `355.0 > 250`. **No read-arc, no
engine change, no `__assets` in guard scope.**

**Both modes coexist.** Field references (constant, typed, control-flow-capable)
for small/scalar use; `<alias>.json` staging for bulk in-code processing — a node
can use both. This **reverses the "assets are opaque" stance of §4.1 for the
single-record case** — safe because the type is a `Port` and the value is a
compile-time constant, so no Vault/secret or runtime machinery rides along.

**Validation** mirrors producer refs: the field must exist on the asset type's
`Port` with a compatible kind, else a compile error (mirroring `GuardUnresolved` /
`GuardTypeMismatch`).

---

## 6. Version pinning

Mirror `workflow_instances.resource_pins`:

- New column `workflow_instances.asset_pins JSONB`, shape `{alias -> {asset_id,
  version}}`, captured at instance-launch time so asset edits after launch don't
  retroactively change running instances.
- The compiler embeds a stable `asset_id` + pinned `version` in the AIR binding
  (not the ref-key, so renames are safe), symmetric with resource pinning.

---

## 7. Data model / schema sketch

New migrations (additive; existing `resources` rows migrate to
`scope_kind='workspace'`):

```sql
-- Generalize resource ownership: add scope_kind/scope_id, backfill from workspace_id.
-- (resources keeps workspace_id for now as a transitional/denormalized column or
--  drops it after backfill — see implementation notes.)
ALTER TABLE resources ADD COLUMN scope_kind TEXT NOT NULL DEFAULT 'workspace';
ALTER TABLE resources ADD COLUMN scope_id   UUID;  -- backfilled = workspace_id
ALTER TABLE resources ADD COLUMN display_path TEXT;  -- virtual folder prefix
-- unique (scope_kind, scope_id, path) where deleted_at is null

-- Asset types: user-defined schemas, scoped + foldered.
CREATE TABLE asset_types (
    id           UUID PRIMARY KEY,
    scope_kind   TEXT NOT NULL,        -- workspace | project | template
    scope_id     UUID NOT NULL,
    name         TEXT NOT NULL,        -- flat identifier ref-key, ^[a-z][a-z0-9_]*$
    display_name TEXT NOT NULL,
    display_path TEXT,                 -- virtual folder prefix
    fields_json  JSONB NOT NULL,       -- Vec<PortField> (the schema)
    cardinality  TEXT NOT NULL DEFAULT 'collection', -- 'object' | 'collection'
    version      INT  NOT NULL DEFAULT 1,
    created_by   TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at   TIMESTAMPTZ,
    UNIQUE (scope_kind, scope_id, name)  -- enforce where deleted_at is null via partial index
);

-- Assets: named, typed collections.
CREATE TABLE assets (
    id           UUID PRIMARY KEY,
    scope_kind   TEXT NOT NULL,
    scope_id     UUID NOT NULL,
    type_id      UUID NOT NULL REFERENCES asset_types(id),
    ref_key      TEXT NOT NULL,        -- flat identifier, ^[a-z][a-z0-9_]*$
    display_name TEXT NOT NULL,
    display_path TEXT,
    version      INT  NOT NULL DEFAULT 1,  -- bumped on record edits
    created_by   TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at   TIMESTAMPTZ,
    UNIQUE (scope_kind, scope_id, ref_key)  -- partial index where deleted_at is null
);

-- Asset records: schema-validated JSONB rows, versioned with the asset.
CREATE TABLE asset_records (
    asset_id  UUID NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    version   INT  NOT NULL,
    row_idx   INT  NOT NULL,
    data      JSONB NOT NULL,          -- validated against the type's fields_json
    PRIMARY KEY (asset_id, version, row_idx)
);

-- Instance pinning, mirrors resource_pins.
ALTER TABLE workflow_instances ADD COLUMN asset_pins JSONB NOT NULL DEFAULT '{}';
```

> Implementation note: whether `resources.workspace_id` is dropped or kept as a
> denormalized transitional column is left to the implementer; the safe path is
> **keep + backfill `scope_kind/scope_id`** and switch reads/uniqueness to the new
> columns, deferring the drop.

---

## 8. API surface

Extend `/api/v1` symmetric with the existing resource endpoints
(`service/src/handlers/resources.rs`). Every handler is `#[utoipa::path]` and the
OpenAPI client **must be regenerated** (`just dev::openapi`).

Asset **types**:
- `GET    /api/v1/asset-types` — list (scope-resolved, folder-aware)
- `POST   /api/v1/asset-types` — create (validate the schema)
- `GET    /api/v1/asset-types/{id}` — fetch (incl. `fields_json`)
- `PUT    /api/v1/asset-types/{id}` — update schema (enforce **additive-only**, §4.3)
- `DELETE /api/v1/asset-types/{id}` — soft-delete (reject if assets exist, or cascade-guard)

Asset **instances**:
- `GET    /api/v1/assets?type_id=&scope=&folder=` — list
- `POST   /api/v1/assets` — create (type + scope + ref_key + display_path)
- `GET    /api/v1/assets/{id}` — fetch metadata + records (paged)
- `PUT    /api/v1/assets/{id}/records` — replace/append records (bumps version, validates each row)
- `POST   /api/v1/assets/{id}/import-csv` — CSV → records (map columns to fields)
- `POST   /api/v1/assets/{id}/files` — upload a file for a `File` field → S3, returns storage path
- `DELETE /api/v1/assets/{id}` — soft-delete

Scope generalization additionally surfaces an optional `scope`/`folder` filter on
the existing `GET /api/v1/resources`.

---

## 9. Reverse lineage (which runs used an asset)

Three granularities, **not** equally cheap — because consumption is
whole-collection (§5), the platform knows an asset+version was *pinned*, but not
which *record* a run's code actually used.

**Asset-level + version-level — SHIPPED.** Every instance pins
`{alias -> {asset_id, version}}` in `workflow_instances.asset_pins` (§6). A GIN
index (`asset_pins jsonb_path_ops`) backs **`GET /api/v1/assets/{id}/usage`**,
which returns the runs that pinned the asset (newest first, each carrying the
binding `alias` and `version_used`). The asset editor surfaces this as a
**"Used by N runs"** panel linking to each instance. *"Which runs used version 3"*
is the same query plus a `@.version == 3` predicate. Cost: one index + one read
endpoint — the pin already carried everything.

**Record / material-level ("runs that used Copper C110") — DEFERRED**, two
complementary paths (neither invalidates the above):

- **(a) Author-picked-row binding (G2).** When a run is *about* one material
  (e.g. a simulation per material), bind a *row* rather than the whole
  collection. The pin becomes `{asset_id, version, row_key}` and the lineage
  query is the same `asset_pins` jsonpath with a `row_key` predicate —
  structural, no opaque-code instrumentation. **Primary path**: it makes the run
  *parameterized by the material*, which is usually the real intent.
- **(b) Runtime usage reporting.** When a run reads *many* records and you want
  the ones it actually touched, an SDK call (`aithericon.record_used(alias,
  row_key)`) lands a row in an `asset_record_usage(instance_id, asset_id,
  version, row_key)` projection — fed by the same causality projector that backs
  the catalogue. Escape hatch for the read-many case; requires instrumenting node
  code.

**Rejected: one-asset-per-material.** Modeling each material as its own `object`
asset gives record-level lineage for free (each material = an `asset_id`), but
trades away the table/CSV authoring model (§4.2) and explodes the asset count.
The record-addressability paths (a)/(b) keep the collection model intact.

---

## 10. Explicitly deferred (clean additive seams)

None of these are invalidated by the above; each is a later, additive step:

- **External / live data** (a real materials DB): an **import/sync job** that
  lands rows into an asset. The static store (§4.2) *is* the sink.
- **Runtime query / behavioral adapters:** a `Queryable` capability with a
  driver-as-staged-script protocol, optionally warm/leased over `LeaseScope`. When
  this lands, it generates a materialized/queryable table off the JSONB rows.
- **`resource_ref` / `asset_ref` field kinds** (composition). Additive field
  kinds; do not invalidate self-contained records.
- **Per-file pre-staging** of `File` fields to a known local path at compile time
  (v1 carries the S3 storage-path inside the record JSON for the node to fetch;
  see §5).
- **Author-picked-row binding** (G2) and **runtime filter** (G3) — also the
  record-level lineage path, see §9.
- **Record-level reverse lineage** (§9 paths a/b); asset-level usage shipped.
- **Real folders table** (per-folder ACL, movable folder objects).
- **Per-row schema versioning** and **breaking-change migration** of asset types.
- **Custom resource *kinds*** (require backend integration; intentionally closed).

---

## 11. Implementation order (for the build)

1. **Migrations** (§7) — generalize `resources` ownership + folders; new
   `asset_types` / `assets` / `asset_records`; `asset_pins`.
2. **Scope resolution** — a shared resolver computing the visible
   resource/asset/type set for a `(scope_kind, scope_id)` with most-specific-wins
   + incomparable-clash → error. Used by list endpoints, the picker, and compiler
   binding.
3. **Asset Rust model + CRUD handlers + OpenAPI** (§8), reusing `PortField` for
   schemas; additive-only update enforcement (§4.3); CSV import; file upload to
   S3 (reuse the resource/file S3 plumbing).
4. **Compiler consumption** (§5) — node-level asset binding → `InputDeclaration`s;
   version-pin into `asset_pins` at launch, symmetric with `resource_pins`.
5. **Frontend** — asset-type builder (schema editor reusing the field widgets),
   asset table/object builder, CSV importer, file upload, asset picker (node
   binding), virtual-folder browse, scope selector. Regenerate the OpenAPI client.
6. **Tests** — schema validation, additive-only enforcement, scope resolution
   (incl. incomparable-clash error), CSV round-trip, consumption lowering to
   staged inputs, and a demo asset exercised end-to-end where feasible.
