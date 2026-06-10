# Page shell — conventions

Every route page is built from the primitives in `$lib/components/shell`:
`PageShell`, `PageHeader`, `PageTabs`, `FilterPills`. Raw `<h1>` and ad-hoc
`mx-auto max-w-*` containers in `src/routes/**` **fail CI**
(`shell-conventions.test.ts` in this directory — allowlist documented there).

## The band anatomy (the default page shape)

Every standard page is a **pinned header band over a scrolling body**:

- The band is a full-width strip with `bg-card` + `border-b border-border`,
  visually distinct from the body background. Inside the page's width
  container it holds the `PageHeader` (h1 / subtitle / right-aligned actions)
  and — when the page has page-level tabs — the tab row.
- The band is **pinned**: `PageShell` renders a flex column, band `shrink-0`,
  body `flex-1 overflow-y-auto`. Content scrolls beneath the band.
- **Page tabs are GitHub-style underline tabs** (never pills): quiet
  muted-foreground labels with a 2px `border-primary` underline when active.
  The tab row sits FLUSH on the band's bottom edge — `PageShell` pulls it
  down with `-mb-px` so the active underline overlaps the band's `border-b`
  exactly. Use the band's `tabs` snippet for this; never hand-build it.
- Component-level tabs elsewhere (cards, drawers, panels) keep the default
  shadcn pill look. ONLY tabs in the page band use the underline style.

`PageShell` owns all of this. You pass two snippets:

- `band` — the title row (`PageHeader` with title/subtitle/actions). The band
  neutralizes PageHeader's in-flow bottom margin (`[&>header]:mb-0`), so no
  `class="mb-*"` juggling.
- `tabs` — the optional tab row, flush on the band border. Put a `PageTabs`
  (link tabs) or a `Tabs.List variant="underline"` trigger row (state tabs)
  here — never tab *content*, which belongs in `children`.

Pages that don't pass `band`/`tabs` fall back to the legacy in-flow layout
(PageHeader inside the scroll content). That's a graceful default during
migration — **new pages use the band**.

## The rules

1. **One header grid, few body widths.** The band's inner content (title
   row + tabs) ALWAYS aligns to the max-w-6xl grid — the header must sit at
   the same x on every page; navigation never makes the title jump.
   `PageShell width` caps the BODY only: `default`/`wide` (max-w-6xl, the
   standard) · `narrow` (max-w-2xl) form bodies · `full` (no cap)
   full-width detail bodies · `bleed` canvas opt-out. Hand-rolled bands on
   bleed pages must wrap their content in `mx-auto w-full max-w-6xl` too.
2. **One h1 scale** (`PageHeader variant`): `page` = text-2xl for top-level
   pages, `detail` = text-lg for back-linked detail pages. Never a raw `<h1>`.
3. **Tabs — pick by what changes:**
   - URL changes (real subroutes, deep-linkable, view unmounts) → `PageTabs`
     in the band's `tabs` snippet.
   - Same URL, switching content panels (optionally `?tab=` + `replaceState`)
     → shadcn `ui/tabs` with `variant="underline"` on `Tabs.List` + every
     `Tabs.Trigger`, the trigger row in the band's `tabs` snippet, the
     `Tabs.Content` panels in `children`. Wrap `Tabs.Root` around the whole
     `PageShell` (class `h-full gap-0`) so context spans band + body.
   - Same list, switching a *filter* (status/mode scopes) → `FilterPills`
     (`href` mode when the filter lives in searchParams, `onSelect` mode for
     local `$state`).
4. **FilterPills placement — ONE rule:** pills go at the TOP OF THE BODY
   (first element of the scroll content, `class="mb-4"`), never inside the
   band. They are filters, not navigation; the band holds only identity
   (title), navigation (tabs) and primary actions.
5. **Header placement:** the pinned band (`band` snippet) is the default for
   every standard page. The in-flow `PageHeader` inside the scroll content is
   the legacy fallback for not-yet-migrated pages only.
6. **Title:** `PageHeader` sets `<title>{title} | Mekhan</title>` by default.
   Override with `headTitle="..."` or suppress with `headTitle={false}` when a
   deeper component owns the title.
7. **Scroll** belongs to the page, never the body: the root layout's `<main>`
   is `overflow-hidden`. `PageShell` owns this — don't add nested
   `h-full overflow-y-auto` wrappers around it.
8. Preserve every existing `data-testid` when migrating — Playwright depends
   on them. `PageShell testid` / `PageHeader titleTestid` / per-tab `testid`
   exist for this.

## Archetype skeletons

### List page (band, optional filter pills at top of body)

```svelte
<script lang="ts">
	import { PageShell, PageHeader, FilterPills } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import Plus from '@lucide/svelte/icons/plus';

	let status = $state('all');
</script>

<PageShell testid="things-page">
	{#snippet band()}
		<PageHeader title="Things" subtitle="All the things in this workspace.">
			{#snippet actions()}
				<Button><Plus class="size-4" /> New thing</Button>
			{/snippet}
		</PageHeader>
	{/snippet}
	<FilterPills
		class="mb-4"
		active={status}
		onSelect={(v) => (status = v)}
		options={[
			{ value: 'all', label: 'All' },
			{ value: 'active', label: 'Active' }
		]}
	/>
	<!-- list body -->
</PageShell>
```

### Tabbed page (state tabs in the band, `?tab=` deep-link) — see `data/+page.svelte`

`Tabs.Root` wraps the WHOLE shell so the trigger row can live in the band
while the panels stay in the scrolling body.

```svelte
<Tabs.Root value={tab} onValueChange={onTab} class="h-full gap-0">
	<PageShell width="wide" testid="data-page">
		{#snippet band()}
			<PageHeader title="Data" icon={Database} subtitle="..." />
		{/snippet}
		{#snippet tabs()}
			<Tabs.List variant="underline">
				<Tabs.Trigger variant="underline" value="entries" data-testid="data-tab-entries">
					Entries
				</Tabs.Trigger>
			</Tabs.List>
		{/snippet}
		<Tabs.Content value="entries">...</Tabs.Content>
	</PageShell>
</Tabs.Root>
```

State tabs that switch body content with `{#if ...}` instead of
`Tabs.Content` (see `fleet/+page.svelte`) can keep `Tabs.Root` entirely
inside the `tabs` snippet — it then exists only to render the trigger row.

### Layout with link tabs (subroute children scroll below) — see `models/+layout.svelte`

```svelte
<PageShell width="wide" testid="model-pool-page">
	{#snippet band()}
		<PageHeader title="Model Pool" subtitle="..." />
	{/snippet}
	{#snippet tabs()}
		<PageTabs
			testid="model-pool-tabs"
			tabs={[
				{ href: '/models/catalog', label: 'Catalog', icon: LibraryBig, testid: 'model-pool-tab-catalog' },
				{ href: '/models/set', label: 'Set', icon: Boxes, testid: 'model-pool-tab-set' }
			]}
		/>
	{/snippet}
	{@render children()}
</PageShell>
```

### Detail page (band holds back-link + title + actions)

```svelte
<PageShell width="wide" testid="thing-detail">
	{#snippet band()}
		<PageHeader
			title={thing.name}
			variant="detail"
			back={{ href: '/things', label: 'Things' }}
			titleTestid="thing-detail-title"
		>
			{#snippet children()}
				<div class="mt-1 flex items-center gap-2 text-sm text-muted-foreground">
					<Badge variant="secondary">{thing.kind}</Badge>
					<span class="font-mono">{thing.id}</span>
				</div>
			{/snippet}
			{#snippet actions()}
				<Button variant="outline" onclick={save}>Save</Button>
			{/snippet}
		</PageHeader>
	{/snippet}
	<!-- detail body -->
</PageShell>
```

### Canvas / editor page (full-bleed opt-out)

Either wrap with the minimal bleed shell:

```svelte
<PageShell width="bleed" testid="template-editor-page">
	<!-- page owns layout: flex h-full flex-col, absolute inset-0, xyflow, … -->
</PageShell>
```

…or skip the shell entirely (xyflow/Yjs workbenches with bespoke chrome) and
add the route to the `CANVAS_ALLOWLIST` in `shell-conventions.test.ts` with a
one-line reason. Never add padding/scroll around a canvas — xyflow needs a
definite-height unpadded parent. Canvas pages that hand-roll a slim toolbar
should use the band tokens (`bg-card`, `border-b border-border`) so they read
as the same family.
