# Demos

Built-in demo workflows shipped with mekhan. Each subdirectory is a single
template, ready to publish.

## Layout

```
demos/<name>/
  demo.json                   # stable templateId + name + description
  graph.json                  # the WorkflowGraph (JSON), small + structural
  nodes/<node-id>/
    main.py                   # per-node source (real .py — IDE, ruff, type-check all work)
    task.json                 # HumanTask form definition (overlay onto data.steps)
```

`nodes/<id>/task.json` is the HumanTask sidecar: each HumanTask is a
node like any other, so its form definition (the verbose `steps`
block tree with all the form fields and instructions) lives next to
the executable files of other node types. `graph.json` carries
`steps: []` for those nodes and the loader merges the sidecar before
returning. Identifying metadata (label, taskTitle, instructionsMdsvex)
stays inline in `graph.json` so the graph still reads at a glance.

Same on-disk shape as the GitOps `mekhan pull/apply` flow modulo the
sidecar split — a hand `mekhan apply demos/<name>/` against the
shipped fixture would fail at publish ("HumanTask rejects empty
steps") because that path doesn't merge sidecars. Use the in-process
loader (or the startup seeder, which calls the loader) for the
shipped demos.

## Loading

- **From Rust**: `mekhan_service::demos::load_demo(path)` returns the
  `(metadata, graph, files)` triple any `/api/templates` consumer
  expects. `list_demo_dirs(root)` enumerates the directory.
- **From the CLI**: `mekhan apply demos/<name>/` (see
  `service/src/bin/cli/apply.rs`).
- **From tests**: same `load_demo` call — tests against the literal
  shipped demo, no hand-rolled graph drift.

## Currently bundled

- **`invoice-processing/`** — end-to-end "Invoice Processing Demo":
  API-trigger → Start → human review → Python extract → decision →
  either fast-path "Processed" or scope[split → manager approval +
  compliance check → join] → "Approved". Exercises every editor node
  type plus direct slug access in Python steps.

## Seeding

The service-side seeder publishes every demo at startup, idempotent
by `demo.json::templateId`. Toggled via env:

- `MEKHAN__DEMOS__SEED=true` — enable (default in `just dev::up-mekhan`)
- `MEKHAN__DEMOS__DIR=<path>` — override the search root (default `demos`)

A seeded demo whose templateId already exists on the server is left
alone — users can hand-edit through the web editor without the seeder
clobbering their changes.

## Adding a demo

1. Drop a new directory under `demos/`.
2. Mint a stable templateId (UUID) — bake it into `demo.json` so
   re-seeding is idempotent and tests can refer to it.
3. Author `graph.json` either by hand or by exporting from a published
   template: `mekhan pull <template-id> demos/<new-name>/ --format json`.
4. Drop per-node sources into `nodes/<id>/`.
5. The Rust unit test `service::demos::tests::invoice_processing_demo_loads`
   is a template for writing a per-demo "this parses" smoke test.

## Editing a demo

The on-disk fixture is canonical. To edit:

- **Through the web editor**: load the seeded demo, edit, then
  `mekhan pull <templateId> demos/<name>/ --format json` to round-trip
  the change back to disk. The seeder will leave the now-edited row
  alone on next restart (idempotent), so re-seed via DB reset or by
  deleting the row first.
- **On disk**: edit `graph.json` / `nodes/<id>/main.py` directly. To
  publish: `mekhan apply demos/<name>/` (uses the same templateId so
  it bumps a new version), or wipe the existing row and let the
  seeder republish.
