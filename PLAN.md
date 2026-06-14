# Library / Vendor Nodes + Node Presentation — Implementation Plan

> Status: **PLANNED** (build gated). Branch `library-nodes` off `5049db63`, dev slot 6.
> Origin: grill-me design session 2026-06-13. 13 decisions resolved — see `Design decisions` below.

## One-line thesis

**A "library node" (OpenFOAM, mumax3, SU2, …) is a published template wearing a brand, dropped onto the canvas as a `sub_workflow` node.** No new runtime/engine/compiler primitive. Everything new lives in the control plane (template metadata + governance + coordinate resolution) and the editor (palette + branded card + upgrade prompt). Demo 59 (`59-firing-curve-bo`, OpenFOAM `solidDisplacementFoam`) is the execution prototype and the spike target.

---

## Design decisions (resolved, do not relitigate)

| # | Area | Decision |
|---|------|----------|
| 1 | Identity | `template_kind` enum on `workflow_templates`: `workflow \| library_node \| private_child`. Same table, same publish pipeline. |
| 2 | Pinning | Drop **pins to current version** (lockfile-style). `VersionPin::Latest` semantics unchanged (freeze-at-publish); we just default new library drops to Pinned. |
| 3 | Scope | Orthogonal `origin` axis: `system` (seeded, read-only) \| `workspace` \| `community`. `visibility` still owns ACL. |
| 4 | Governance | Explicit role-gated **Promote** action (not a publish checkbox). workspace=Owner/Admin; community=platform-admin review. Symmetric demote. |
| 5 | Customize | **Fork-to-workspace** deep-copy with `forked_from` provenance (+ future rebase hint). |
| 6 | Discovery | Palette = Primitives / Library split. Library grouped by controlled **category** vocab + free-text **vendor** + search + recently-used. |
| 7 | Identity-2 | Stable `vendor/slug` **coordinate** (`openfoam/solid-displacement`), decoupled from UUID family. GitOps/upgrade/refs resolve by coordinate. |
| 8 | Upgrades | Safe-vs-breaking **auto-classified from derived IO contract diff** (input removed/retyped or new-required = breaking). Point at exact `input_mapping`s. No hand-set semver. |
| 9 | Icons | **Named icon registry** now (Lucide + bundled vendor set, string key). Asset-backed custom icons later (`<img>`, never inline SVG). color = hex/token. |
| 10 | Distribution | Extend demo/GitOps shape: `demo.json` gains `templateKind`/`origin`/`coordinate`/`presentation`. Folder = pack. Seeded idempotently like demos. |
| 11 | Lifecycle | `status: active \| deprecated \| retired` + `superseded_by`. Version rows **never hard-deleted** → pinned embeds always resolve. |
| 12 | Canvas | Dropped library node **IS a `sub_workflow`** node stamped `sourceCoordinate` + frozen `presentation`. Compiler/engine **unchanged**. "Detach from library" clears the stamp. |
| 13 | Theming | Full presentation (icon+color+vendor, frozen) on library nodes. ANY node gets optional lightweight `{accentColor, label}` — **no** per-instance icon picker. |

---

## Grounding — the load-bearing existing code

- **Visual metadata today is frontend-only, keyed by node *kind***: `app/src/lib/editor/node-palette-meta.ts` (~L48-76, the `META` map of `{icon, color}`). No per-template/per-instance field anywhere.
- **SubWorkflow node renders blind**: `app/src/lib/components/editor/nodes/SubWorkflowNode.svelte:26-28` shows `data.templateId.slice(0,8)` + generic `Workflow` icon. Child name never stored on the node.
- **Node card**: `app/src/lib/components/editor/nodes/WorkflowNodeCard.svelte` (`tv()` variants per kind, `ICON_BG` map). Today icon/color come from the kind, not the node.
- **Node component registry**: `app/src/lib/components/editor/nodes/index.ts` (`nodeTypes`).
- **Subworkflow data model**: `service/src/models/template/graph.rs` (`SubWorkflow` variant ~L847-901: `template_id`, `version_pin`, `input_mapping`, `output`, `input_contract`).
- **Subworkflow compile**: `service/src/compiler/subworkflow.rs` (`derive_child_io` L208-264, `make_child_callable` L121-206).
- **Subworkflow resolve @ publish**: `service/src/process/publish.rs` (`resolve_subworkflow_air` ~L1457-1618 → `ResolvedChild { air, resolved_version, template_id, input_contract, output_contract }`).
- **Template row**: `service/src/models/template/graph.rs` `WorkflowTemplate` (~L24-97): `base_template_id`, `version`, `is_latest`, `published`, `visibility`, `owner_template_id`, `interface_json`.
- **Node registry (backend)**: `service/src/nodes/mod.rs` (`NodeDecl`, `NODES` slice, `NodeDescriptor`) → `GET /api/v1/node-types` (`service/src/handlers/node_types.rs`). Primitives only.
- **Demos = on-disk template shape**: `service/src/demos.rs` (`DemoMetadata` L56-93, `load_demo` L191-244, `merge_task_sidecars`). `demos/README.md`. Spike target: `demos/59-firing-curve-bo/`.
- **Catalogue/facets prior art** (reuse for palette facets if useful): see `project_catalogue_query_interface` memory.

---

## Phasing

Backend-first. Each phase independently shippable. **Phase 1 (spike) proves the round-trip on demo 59 before any library/governance surface is built.**

### Phase 0 — Schema & DTO foundation
*Goal: persist the new fields; no behavior yet.*

1. **Migration** `service/migrations/20240183000000_library_nodes.sql`. (Branch rebased onto main `9f2db246` 2026-06-13 — multi-tenancy + entity-pages + analytics all merged; latest migration is now `20240182000000`, so `20240183000000` is the next free.) **MT note:** `workspace_id` is now pervasive — `WorkflowTemplate`/`DemoMetadata`/publish all carry it; thread it where required:
   - `workflow_templates.template_kind TEXT NOT NULL DEFAULT 'workflow'` (`workflow|library_node|private_child`). Backfill: rows with `owner_template_id IS NOT NULL` → `private_child`.
   - `workflow_templates.origin TEXT` (`system|workspace|community`, NULL for plain workflows).
   - `workflow_templates.coordinate TEXT` — `vendor/slug`. Unique index **scoped** `(origin, coordinate) WHERE coordinate IS NOT NULL`.
   - `workflow_templates.presentation JSONB` — `{icon, color, vendor, category, badge}`.
   - `workflow_templates.lifecycle_status TEXT NOT NULL DEFAULT 'active'` (`active|deprecated|retired`).
   - `workflow_templates.superseded_by TEXT` (coordinate).
   - `workflow_templates.forked_from JSONB` — `{coordinate, template_id, version}`.
   - ⚠️ Migration-number collision risk (see analytics + entity-pages reservations). Pick the next free and grep before committing.
2. **Rust model**: extend `WorkflowTemplate` (`graph.rs`) with the new columns. **Critical**: every explicit `SELECT` list that builds a `WorkflowTemplate` must add the new columns (the `updated_by`/resolver-SELECT footgun bit twice before — see `project_iam_granular_plan` + `project_e2e_worker_partition_env` memories). Grep `resource_resolver.rs` and all `sqlx::query_as` against `workflow_templates`.
3. **Presentation DTO**: new `Presentation { icon: String, color: Option<String>, vendor: Option<String>, category: Option<String>, badge: Option<String> }` (`ToSchema`).
4. **Per-instance accent (decision 13)**: add optional `accent_color: Option<String>` to the common node-data envelope (or each variant's shared fields) in `graph.rs`. Label already exists. No icon field.
5. `just dev::openapi` regen (hard contract — CI `openapi-drift` gate).

### Phase 1 — SPIKE: brand the subworkflow card end-to-end on demo 59
*Goal: prove the whole round-trip with the smallest vertical slice, before building library/palette/governance.*

1. **Node data stamp**: add `source_coordinate: Option<String>` + `presentation: Option<Presentation>` to the `SubWorkflow` variant (`graph.rs`).
2. **Resolve-time freeze**: in `resolve_subworkflow_air` (`publish.rs`), when the resolved child has `template_kind=library_node`, copy its `coordinate` + `presentation` into the snapshot written back onto the node (alongside the existing `input_contract`/`output` snapshot). Same mechanism that already freezes the IO contract.
3. **Frontend render**: `SubWorkflowNode.svelte` — when `data.presentation` present, render `presentation.vendor`/child name + registry icon + accent color instead of the UUID slice. `WorkflowNodeCard.svelte` + `node-palette-meta.ts` accept a per-instance icon/color override (signature change: `icon`/`color` props win over kind defaults).
4. **Icon registry**: `app/src/lib/editor/icon-registry.ts` — `Record<string, Component>` mapping string keys → Svelte/Lucide components, with a safe fallback. Seed with a couple vendor icons (openfoam) + reuse Lucide.
5. **Spike fixture**: hand-add `templateKind: "library_node"`, `origin: "system"`, `coordinate: "openfoam/firing-curve"`, `presentation: {...}` to `demos/59-firing-curve-bo/demo.json`; create a tiny consumer demo that embeds it as a subworkflow. Reseed (`mekhan demos reseed`), publish the consumer, confirm the branded card renders on canvas + instance view.
6. **GATE: live-verify on slot 6** before proceeding. This validates decisions 9, 12, 13 + the freeze path. (Heed `feedback_verify_the_untouched_ui_path` — click-Run the real form, don't trust fixtures.)

> **✅ SPIKE VERIFIED 2026-06-13 (slot 6).** Backend migration→seed→row→io-contract proven via curl (presentation/coordinate/name returned). Branded `SubWorkflowNode` card renders on a clean editor load for demo 06 (vendor "Aithericon", `hand-helping` icon, teal accent, "demo" badge) — chain: graph.json → Rust `yjs_encode` → ydoc → read path → render. io-contract live-freeze path also confirmed (rebrands on node selection).
>
> **⚠️ KEY FINDING — freeze at PUBLISH time, not just editor live-edit.** The editor's io-contract live-freeze only *persists* on a **draft (editable)** template; **public+published** templates are read-only in Yjs (`9deaf3e1`) and silently drop the write — so the freeze rendered in-session but didn't survive reload until presentation was baked into the seed graph. **Therefore the real feature MUST stamp `sourceCoordinate`+`presentation` onto the embedding node at PUBLISH time in the backend** (`resolve_subworkflow_air` in `publish.rs` already resolves the child row — copy its coordinate/presentation onto the node snapshot there, the same place it could reconcile the IO contract). The editor live-freeze stays as the draft-time convenience. Add this to Phase 1 proper. The spike proved the render + seed/yjs_encode path; the publish-time stamp is the missing robustness piece.

### Phase 2 — Distribution + seeding (decisions 1, 3, 7, 10) — ✅ DONE
1. `DemoMetadata` (`demos.rs`) gains `template_kind`, `origin`, `coordinate`, `presentation` (serde-optional, default `workflow`/none). `load_demo` threads them onto the publish call. *(done in P1.)*
2. Seeder sets `template_kind`/`origin=system`/`coordinate`/`presentation`/`lifecycle_status=active` on seeded library nodes. Idempotent on `templateId` as today. *(done in P1.)*
3. Coordinate uniqueness enforced at seed + publish (friendly error on dup within `(origin)`). *(done: `DemoSeedError::CoordinateConflict` + `LibraryNodeMissingCoordinate`, pre-insert SELECT guard.)*
4. Author 1–2 real system packs as demo-shaped dirs (OpenFOAM solidDisplacementFoam as a clean reusable node; optionally a second vendor) to exercise grouping. *(done: `demos/openfoam-solid-displacement` — Start firing curve → solidDisplacementFoam step → End objectives, coordinate `openfoam/solid-displacement`, Wind/CFD branding; reuses demo 59's physics with `cand.*`→`input.*`.)*

> **✅ PHASE 2 COMPLETE 2026-06-14.** Commits `0b3bf9ef` (chore: regen 4 stale form-definition AIR goldens — pre-existing branch drift, unrelated) + `3410672c` (feat: P2). Two extras beyond the plan: (a) **unique-index bug fix** — `uq_workflow_templates_origin_coordinate` was unscoped `(origin, coordinate)`, which would reject a library node's 2nd version; scoped to `is_latest` (decision 11 — version rows coexist forever). (b) new **`library` demo category** ("Library Nodes") so the pack files under `/demos/library`. Offline-green: air_snapshots (incl. new `openfoam-solid-displacement` golden, 3536-line AIR), 33 demos lib tests, cargo check. **Live-verified slot 6**: reset → seed 78/0-fail; both library-node rows (`aithericon/hello-world` + `openfoam/solid-displacement`) carry template_kind/origin/coordinate/presentation/lifecycle_status; index predicate confirmed `is_latest`-scoped; node filed under Library Nodes. The coordinate-guard SQL runs for BOTH library nodes during every seed (demo 01 is also a library_node) so a 0-fail seed proves the guard is well-formed. Slot 6 torn down; slot-0 Ollama (:11434) never touched (OLLAMA_PORT=11534). No OpenAPI change (internal error enum only).

### Phase 3 — Library catalogue API + palette (decisions 6, 7, 11) — ✅ DONE
1. **Endpoint** `GET /api/v1/node-library` → `Vec<LibraryNodeDescriptor>` (coordinate, name, vendor, category, presentation, origin, lifecycle_status, current version, template family id). ACL-filtered by `visibility`/workspace; excludes `retired` (include `deprecated` with flag). Mirror the `node-types` handler shape. *(done: `handlers/node_library.rs`, `?include_deprecated`, ordered category→vendor→name, `rename_all=camelCase` to match `NodeDescriptor`.)*
2. **Category vocabulary**: small controlled enum (CFD, Micromagnetics, ML, Robotics, Data, …) — extensible constant, validated on promote/seed. Vendor stays free text. *(done: `LIBRARY_CATEGORIES` + `is_known_library_category` in the template model; seed validates `presentation.category` → `DemoSeedError::{LibraryNodeMissingCategory,UnknownLibraryCategory}`; unit-tested.)*
3. **Palette UI**: extend the add-node palette to two sections — Primitives (`/api/v1/node-types`) + Library (`/api/v1/node-library`), grouped by category → vendor, with search + recently-used. Dropping a library node creates a `sub_workflow` node pre-pinned to the coordinate's current version, presentation pre-stamped from the descriptor. *(done: `library-registry.svelte.ts` + `NodePalette.svelte` two-section/Recent→category→vendor + `WorkflowCanvas` onDrop enrichment. **Gotcha:** marking "recent" at dragstart re-renders the palette and detaches the in-flight dragged element → drag hangs; mark on DROP instead. Caught by the e2e.)*
4. `just dev::openapi` regen. *(done.)*

> **✅ PHASE 3 COMPLETE 2026-06-14.** Commits `fe1af540` (P3a backend: endpoint + vocab + seed validation, openapi/schema regen) + `67536086` (P3b palette UI + branded drop + e2e). Built on top of a **main merge** (`941264aa`, after renumbering my migration `20240183`→`20240184` to dodge main's `workspace_archived`). Live (slot 6, real backend): `GET /api/v1/node-library` returns both seeded packs camelCase, category-ordered; new Playwright spec `library-palette.test.ts` 2/2 — Library section renders from the live catalogue, dropping `openfoam/solid-displacement` yields a sub-workflow card showing vendor "OpenFOAM" + pin "v1". Gates: demos 33/33, air_snapshots 31/31, vocab unit test, clippy clean (only the pre-existing `register_catalogue_entry` 13/12 from main's MT work), svelte-check 0/0, vitest 870.

### Phase 4 — Governance: promote / demote / fork (decisions 4, 5)
1. **Promote** `POST /api/v1/templates/{id}/promote` body `{ origin, coordinate, presentation, category }`. Role gate: workspace=Owner/Admin (reuse `grants.rs` resolver / `my_effective_role`); community=platform-admin (review path — for v1 could be a flag + admin-only role; full review queue deferrable). Sets `template_kind=library_node`. Audit-logged (reuse Phase-2 audit infra from `project_iam_granular_plan`).
2. **Demote** `POST /api/v1/templates/{id}/demote` — reverse, role-gated. Existing embeds unaffected (frozen).
3. **Fork** `POST /api/v1/library/{coordinate}/fork` → deep-copy template family into new family, `origin=workspace`, editable, `forked_from={coordinate,template_id,version}`. Reuse `new_version`/copy plumbing. Returns new family id; user edits + can re-promote.
4. **UI**: "Promote to library node" action on a published template (presentation editor: icon picker from registry, color, vendor, category, coordinate). "Fork to workspace" on read-only system/community nodes. Both role-gated in UI + server.

### Phase 5 — Upgrade prompt + lifecycle (decisions 2, 8, 11)
1. **Upgrade detection**: a node pinned to `coordinate@vN` where a newer published version exists → editor surfaces "vN+1 available". Resolve by coordinate → family → latest published version.
2. **Contract-diff classifier**: reuse `derive_child_io` on both versions; classify breaking (consumed input removed/retyped, or new required input) vs compatible. Endpoint `GET /api/v1/library/{coordinate}/upgrade-preview?from=N` → `{ classification, contract_diff, affected_input_mappings }`.
3. **Upgrade UI**: prompt shows safe/breaking + (breaking) which `input_mapping`s need remap; on adopt, re-pin + re-snapshot contract/presentation. Reuse the existing SubWorkflow input-mapping editor for the remap step.
4. **Lifecycle**: deprecate/retire admin actions set `lifecycle_status` + `superseded_by`. Palette hides retired, warns on deprecated with successor link. Never hard-delete version rows.
5. **Rebase hint (forks)**: if `forked_from` and upstream has a newer version, surface an informational "upstream vX available" (no auto-merge in v1).

---

## Cross-cutting / gotchas (from prior arcs — heed these)

- **SELECT-list footgun**: adding columns to `WorkflowTemplate` without updating every explicit SELECT (esp. resolvers) silently breaks publish/seed for whole demo set. Grep exhaustively. (`project_iam_granular_plan`.)
- **Migration numbering collision**: analytics (`20240175`) + entity-pages (`20240175000000` reserved) are in flight on other branches. Pick the next free number and re-check at merge time. (`project_catalogue_datatypes` renumber pain.)
- **OpenAPI is a hard contract**: regen after every `#[utoipa::path]`/DTO change or CI `openapi-drift` fails.
- **sccache + migration rebuild**: touching a migration may need a forced rebuild (`touch` pool.rs / `RUSTC_WRAPPER='' cargo build`) for the new migration to take. (`project_catalogue_producers`.)
- **Build the right binary**: mekhan-service builds from umbrella root → `./target/`. Edit THIS worktree, not primary. `cargo check ≠ test rebuild` (stale binary). (`project_agent_dispatch_partition_bug`.)
- **Verify the untouched UI path**: live-verify by clicking the real, empty form — not just fixtures. (`feedback_verify_the_untouched_ui_path`.)
- **Engine/compiler stay untouched** — if a change seems to need the engine, stop: the design says it shouldn't.

## Suggested commit boundaries
P0 schema+DTO · P1 spike (brand demo 59) · P2 distribution · P3 catalogue+palette · P4 governance · P5 upgrades+lifecycle. Each on `library-nodes`, merge `--no-ff` to local main when the arc is green (unpushed per repo convention until asked).

## Open / deferred
- Community **review queue** (Phase 4 community promote) — v1 may ship admin-flag only.
- Asset-backed custom icons (decision 9 "later").
- Pack manifest (`pack.json`) for multi-node vendor grouping (decision 10 chose folder=pack; manifest deferred).
- Semver intent layer + contract guard (decision 8 chose pure auto-classify; "both" deferred).
- Fork **rebase/merge** automation (Phase 5 gives hint only).
