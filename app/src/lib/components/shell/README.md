# Page shell — conventions

Every route page is built from the primitives in `$lib/components/shell`:
`PageShell`, `PageHeader`, `PageTabs`, `FilterPills`. Raw `<h1>` and ad-hoc
`mx-auto max-w-*` containers in `src/routes/**` **fail CI**
(`shell-conventions.test.ts` in this directory — allowlist documented there).

## The rules

1. **One width per archetype** (`PageShell width`):
   `narrow` (max-w-2xl) forms/profile · `default` (max-w-5xl) lists ·
   `wide` (max-w-6xl) dense operator surfaces (fleet, data, models) ·
   `full` (no cap) full-width detail · `bleed` canvas opt-out.
2. **One h1 scale** (`PageHeader variant`): `page` = text-2xl for top-level
   pages, `detail` = text-lg for back-linked detail pages. Never a raw `<h1>`.
3. **Tabs — pick by what changes:**
   - URL changes (real subroutes, deep-linkable, view unmounts) → `PageTabs`.
   - Same URL, switching content panels (optionally `?tab=` + `replaceState`)
     → shadcn `ui/tabs` (`Tabs.Root` state tabs).
   - Same list, switching a *filter* (status/mode scopes) → `FilterPills`
     (`href` mode when the filter lives in searchParams, `onSelect` mode for
     local `$state`).
4. **Header placement:** in-flow `PageHeader` inside the scroll content is the
   default. The pinned band (`PageShell` `band` snippet) is ONLY for layouts
   whose children scroll under a fixed header + tab bar (e.g. `/models`,
   `/instances/[id]`).
5. **Title:** `PageHeader` sets `<title>{title} | Mekhan</title>` by default.
   Override with `headTitle="..."` or suppress with `headTitle={false}` when a
   deeper component owns the title.
6. **Scroll** belongs to the page, never the body: the root layout's `<main>`
   is `overflow-hidden`. `PageShell` owns this — don't add nested
   `h-full overflow-y-auto` wrappers around it.
7. Preserve every existing `data-testid` when migrating — Playwright depends
   on them. `PageShell testid` / `PageHeader titleTestid` / per-tab `testid`
   exist for this.

## Archetype skeletons

### List page (`default` width, optional filter pills)

```svelte
<script lang="ts">
	import { PageShell, PageHeader, FilterPills } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import Plus from '@lucide/svelte/icons/plus';

	let status = $state('all');
</script>

<PageShell testid="things-page">
	<PageHeader title="Things" subtitle="All the things in this workspace.">
		{#snippet actions()}
			<Button><Plus class="size-4" /> New thing</Button>
		{/snippet}
	</PageHeader>
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

### Detail page (back-link, `detail` h1, meta row)

```svelte
<PageShell width="wide" testid="thing-detail">
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
	<!-- detail body -->
</PageShell>
```

### Tabbed page (state tabs, `?tab=` deep-link) — see `data/+page.svelte`

```svelte
<PageShell width="wide" testid="data-page">
	<PageHeader title="Data" icon={Database} subtitle="..." />
	<Tabs.Root value={tab} onValueChange={onTab}>
		<Tabs.List class="mb-4">
			<Tabs.Trigger value="entries" data-testid="data-tab-entries">Entries</Tabs.Trigger>
		</Tabs.List>
		<Tabs.Content value="entries">...</Tabs.Content>
	</Tabs.Root>
</PageShell>
```

### Layout with pinned band + link tabs (subroute children scroll below)

```svelte
<PageShell width="wide" testid="model-pool-page">
	{#snippet band()}
		<PageHeader title="Model Pool" subtitle="..." class="mb-3" />
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
definite-height unpadded parent.
