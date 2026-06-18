---
type: Database Domain
title: Collaboration (Yjs)
description: The Yjs CRDT update log and snapshot store backing collaborative editing of template graphs and pages.
tags: [database, yjs, crdt, collaboration, snapshots]
timestamp: 2026-06-18T00:00:00Z
---

# Collaboration (Yjs)

Collaborative editing (the template graph editor and Edra/Tiptap
[pages](templates-and-authoring.md)) is backed by Yjs CRDTs. mekhan runs the Yjs
sync server over the `/api/yjs/{doc_id}` WebSocket (binary protocol, not
OpenAPI-modeled); these two tables persist the document state.

# Schema

| Table | Key columns | Purpose |
|-------|-------------|---------|
| `yjs_documents` | `id`, `doc_id`, `seq` (sequence), `update_data` (bytea), `doc_kind` (`graph` default) | Append-only log of Yjs binary updates per document; `seq` orders them. |
| `yjs_snapshots` | `id`, `doc_id`, `snapshot_data` (bytea), `snapshot_seq`, `doc_kind` | Periodic compacted snapshots so a client doesn't replay the full update log on join. |

# Notes

- `doc_id` is the logical document (a template's graph or a page body); it is
  **not** the same as `template_id` — pages and graphs both produce Yjs docs.
- `doc_kind` distinguishes graph documents from page documents.
- This is a pure CRDT store: no foreign keys, and the authoritative compiled
  graph still lands in [`workflow_templates.graph`](templates-and-authoring.md)
  on save.

# Citations

[1] `service/migrations/20240103_yjs_documents.sql`.
[2] `service/src/yjs/`, CLAUDE.md `/api/yjs/{template_id}` notes.
