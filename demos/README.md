# Demos

Built-in demo workflows shipped with mekhan. Each subdirectory is a single
template, ready to publish.

## Layout

```
demos/<name>/
  .mekhan.json         # stable templateId + name + description
  graph.json           # the WorkflowGraph (JSON)
  nodes/<node-id>/
    main.py            # per-node source files (real .py — IDE, ruff, type-check all work)
```

Same on-disk shape as the GitOps `mekhan pull/apply` flow, so a demo
directory IS a publishable template — you can hand-edit one and
`mekhan apply demos/invoice-processing/` to push it.

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

## Adding a demo

1. Drop a new directory under `demos/`.
2. Mint a stable templateId (UUID) — bake it into `.mekhan.json` so
   re-seeding is idempotent and tests can refer to it.
3. Author `graph.json` either by hand or by exporting from a published
   template: `mekhan pull <template-id> demos/<new-name>/ --format json`.
4. Drop per-node sources into `nodes/<id>/`.
5. The Rust unit test `service::demos::tests::invoice_processing_demo_loads`
   is a template for writing a per-demo "this parses" smoke test.
