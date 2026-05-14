# UI primitives

This directory holds the design-system primitives (shadcn-svelte style). Everything
outside this directory should consume them — not roll its own.

## Conventions

1. **Primitives are canonical.** If a component fits an existing primitive (`Button`,
   `Badge`, `Card`, `Input`, `Textarea`, `Separator`, …), use it. If the variant you
   need doesn't exist yet, **extend the primitive** — don't hand-roll the styling
   somewhere else.

2. **Theme tokens only.** Colors come from the theme tokens defined in
   `app/src/routes/layout.css` — `--primary`, `--secondary`, `--muted`, `--accent`,
   `--destructive`, `--success`, `--warning`, `--info`, `--border`, `--input`,
   `--ring`, `--card`, `--popover`, `--background`, `--foreground`. Use them via
   Tailwind utilities (`bg-success`, `text-warning`, `border-info`, etc.).

   **Do not** write literal Tailwind palette colors (`bg-green-500`, `text-red-400`,
   `border-amber-300`, …) outside this directory. They bypass the theme and decouple
   feature code from light/dark and brand updates.

3. **Petri concepts → `NodeKindBadge`.** Place kinds, transition kinds, event types,
   and coordination signal types have a single source of truth in
   `app/src/lib/components/petri/NodeKindBadge.svelte`. Use it; don't duplicate the
   color/label map.

4. **One Card pattern.** For muted-background informational blocks use
   `<Card tone="muted">`. For bordered card containers, `<Card>` (default tone)
   plus `<CardHeader>` / `<CardTitle>` / `<CardContent>` / `<CardFooter>`.

## Guardrail

```
pnpm lint:ui
```

`scripts/lint-ui.mjs` scans `app/src` (excluding `lib/components/ui/`) and fails
on **new** violations of:

- Literal Tailwind palette colors (`bg-/text-/border-/ring-{family}-NNN`)
- Raw `<input>` / `<textarea>` in `.svelte` files (use `<Input>` / `<Textarea>`)

Pre-existing violations are captured in `scripts/lint-ui.baseline.json`; the rule
only fails when a file's bucket grows above its baselined count. The baseline is
the working punch-list — drive it toward zero over time.

### Allow-list

If a violation is genuinely needed (third-party widget skin, brand-specific
diagram color, etc.), add a comment containing `ui-allow: <reason>` on the same
line or the line above:

```svelte
<!-- ui-allow: xyflow handle anchor requires literal color for SVG diff -->
<Handle class="!border-2 !border-blue-500 !bg-white" />
```

### Re-baselining

After a deliberate refactor that removes (or sometimes adds) violations:

```
pnpm lint:ui --update-baseline
```

Commit the updated `scripts/lint-ui.baseline.json` alongside the refactor.

## Extending variants

When you need a new visual treatment:

1. Open the primitive's `.svelte` file (e.g. `badge/badge.svelte`).
2. Add a new variant entry referencing theme tokens, e.g.
   ```ts
   accent: "bg-accent text-accent-foreground border-transparent",
   ```
3. Export the new variant via the `index.ts` re-exporter if it's a public API.
4. If the new concept is petri-domain (a new place kind, new event type), extend
   `NodeKindBadge`'s `KIND_MAP` instead of adding a badge variant.

## Primitives

Current set: `badge`, `block-chart`, `button`, `calendar`, `card`, `chart`,
`checkbox`, `copy-button`, `file-drop-zone`, `input`, `label`, `popover`,
`radio-group`, `rating-group`, `select`, `separator`, `sheet`, `signature-pad`,
`sonner`, `spinner`, `textarea`, `tooltip`.

Missing primitives worth considering (driven by current violations):

- **Skeleton** — hand-rolled loading states exist in editor panels.
- **FormField** — label + input + error message bundle; many `<label>` + `<input>`
  pairs in `editor/panels/property-sections/automated/*` would collapse.
- A `Button` `inline` size that drops height/padding for text-flow link buttons
  (would let us migrate the remaining ~30 `text-primary hover:underline` buttons
  in `petri/Inspector.svelte`).
