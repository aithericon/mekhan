<script lang="ts">
	// The Fields reference section of the Entries rail, scope-aware: meta.*
	// fields carry `applies_to` (the probed formats they're meaningful for),
	// so when the applied query asserts formats the reference narrows to
	// those formats' detail fields instead of listing every probe leaf.
	// Discovery metadata only — the server accepts any field regardless.
	import type { QueryFieldDesc, QueryFieldsResponse } from '$lib/api/data';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';

	let {
		registry,
		activeFormats,
		onInsert
	}: {
		/** Server field registry (null while loading). */
		registry: QueryFieldsResponse | null;
		/** Formats asserted by the APPLIED query (lowercased, deduped). */
		activeFormats: string[];
		/** Called with the draft stub to insert, e.g. `meta.delimiter:`. */
		onInsert: (term: string) => void;
	} = $props();

	let open = $state(false);
	// Per-format chevron toggles — only rendered when no format is asserted.
	let expandedFormats = $state<Record<string, boolean>>({});

	const universalMeta = $derived(
		(registry?.meta ?? []).filter((f) => (f.applies_to ?? []).length === 0)
	);
	// A field may appear under several formats (shared leaves like
	// meta.compression span parquet + images + archives).
	const byFormat = $derived.by(() => {
		const map = new Map<string, QueryFieldDesc[]>();
		for (const f of registry?.meta ?? []) {
			for (const fmt of f.applies_to ?? []) {
				const list = map.get(fmt);
				if (list) list.push(f);
				else map.set(fmt, [f]);
			}
		}
		return map;
	});
	const formatKeys = $derived([...byFormat.keys()].sort());
	const scopedFormats = $derived(activeFormats.filter((fmt) => byFormat.has(fmt)));
	// Narrowing must not look like fields vanished — when formats are active
	// and other per-format groups are hidden, say so.
	const hasHiddenFormats = $derived(
		activeFormats.length > 0 && scopedFormats.length < byFormat.size
	);
</script>

{#snippet fieldRows(items: QueryFieldDesc[])}
	{#each items as f (f.name)}
		<button
			type="button"
			class="flex w-full items-baseline gap-2 rounded px-1 py-0.5 text-left text-sm hover:bg-accent"
			title={f.description}
			onclick={() => onInsert(`${f.name}:`)}
		>
			<span class="truncate font-mono text-foreground">{f.name}</span>
			<span class="ml-auto shrink-0 text-xs text-muted-foreground">{f.value_type}</span>
		</button>
	{/each}
{/snippet}

<section data-testid="rail-fields">
	<button
		type="button"
		class="flex w-full items-center gap-2 text-sm font-medium text-foreground"
		onclick={() => (open = !open)}
	>
		{#if open}
			<ChevronDown class="size-4 text-muted-foreground" />
		{:else}
			<ChevronRight class="size-4 text-muted-foreground" />
		{/if}
		Fields
	</button>
	{#if open}
		<div class="mt-2 max-h-72 overflow-y-auto">
			{#if !registry}
				<p class="px-1 py-1 text-xs text-muted-foreground">Loading field registry…</p>
			{:else}
				{#each [{ label: 'Fields', items: registry.native }, { label: 'Metadata (meta.*)', items: universalMeta }] as group (group.label)}
					<p class="px-1 pb-1 pt-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
						{group.label}
					</p>
					{@render fieldRows(group.items)}
				{/each}
				{#if activeFormats.length > 0}
					{#each scopedFormats as fmt (fmt)}
						<p class="px-1 pb-1 pt-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
							{fmt}
						</p>
						{@render fieldRows(byFormat.get(fmt) ?? [])}
					{/each}
					{#if hasHiddenFormats}
						<p class="px-1 pb-1 pt-1.5 text-xs text-muted-foreground">
							scoped to format: {activeFormats.join(', ')}
						</p>
					{/if}
				{:else}
					{#each formatKeys as fmt (fmt)}
						<div>
							<button
								type="button"
								class="mt-0.5 flex w-full items-center gap-1.5 rounded px-1 py-0.5 text-sm font-medium text-foreground hover:bg-accent"
								onclick={() => (expandedFormats[fmt] = !expandedFormats[fmt])}
								data-testid={`rail-fields-format-${fmt}`}
							>
								{#if expandedFormats[fmt]}
									<ChevronDown class="size-3.5 text-muted-foreground" />
								{:else}
									<ChevronRight class="size-3.5 text-muted-foreground" />
								{/if}
								{fmt}
							</button>
							{#if expandedFormats[fmt]}
								<div class="pl-2">
									{@render fieldRows(byFormat.get(fmt) ?? [])}
								</div>
							{/if}
						</div>
					{/each}
				{/if}
				<p class="px-1 pb-1 pt-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
					Metadata containment
				</p>
				{#each registry.containment as c (c.term)}
					<button
						type="button"
						class="flex w-full items-baseline gap-2 rounded px-1 py-0.5 text-left text-sm hover:bg-accent"
						title={c.description}
						onclick={() => onInsert(`${c.term}:`)}
					>
						<span class="truncate font-mono text-foreground">{c.term}:</span>
					</button>
				{/each}
			{/if}
		</div>
	{/if}
</section>
