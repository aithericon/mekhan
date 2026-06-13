# Edra (vendored, trimmed)

A lean, shadcn-styled Tiptap v3 + Svelte 5 rich-text editor, adapted from
[**Edra**](https://github.com/Tsuzat/Edra) by Tsuzat (MIT — see `LICENSE`).

## Provenance

- Upstream: <https://github.com/Tsuzat/Edra> — `next` branch (Tiptap v3 line).
- This is **not** a verbatim copy. Because pulling the full Edra component
  registry headlessly was impractical, this is a **lean equivalent** built to
  Edra's shape and conventions (extension set + shadcn toolbar/bubble menu)
  using *this* app's own `$lib/components/ui/*` shadcn primitives and Tailwind.
- Trimmed: **dropped** media-upload, image, YouTube/video, and slash/command
  menus. **Kept** StarterKit (history disabled — Yjs owns undo), headings,
  lists, blockquote, horizontal rule, link, code block (lowlight highlighting),
  tables, and a placeholder.
- All ProseMirror imports go through `@tiptap/pm/*` so y-prosemirror shares a
  single ProseMirror copy (also enforced by `pnpm.overrides` in
  `app/package.json`).

## Layout

- `extensions.ts` — the trimmed extension set + a `pageExtensions()` factory
  that binds Collaboration to a caller-supplied `Y.XmlFragment`.
- `EdraEditor.svelte` — the editor shell: renders the ProseMirror DOM into an
  element and exposes the constructed `Editor` via an `onready` callback. It is
  **transport-agnostic** — it never creates a Y.Doc/provider itself; the caller
  (`$lib/components/pages/PageEditor.svelte`) owns the sync-then-bind lifecycle.
- `EdraToolbar.svelte` — a shadcn-styled formatting toolbar (bold/italic/strike,
  headings, lists, blockquote, link, inline code, code block, table).
- `EdraBubbleMenu.svelte` — a floating selection bubble menu (bold/italic/link/code).
- `commands.ts` — small command helpers shared by the toolbar + bubble menu.
- `content.css` — editor content styles (scoped to `.edra-content`; this app
  has no `@tailwindcss/typography`/`prose`, so styles are self-contained).
