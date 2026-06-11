<script lang="ts">
	// The Data-types rail section: registered types as one-click query entries
	// (`datatype:` terms) with their server-derived column projection
	// expandable inline. Promotion happens in the Schemas facet group; this
	// section edits / deletes what exists.
	import type { CatalogueDataType } from '$lib/api/data';
	import type { DataTypesState } from './data-types.svelte';
	import { quoteIfNeeded } from './query-language';
	import DataTypeDialog from './DataTypeDialog.svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { toast } from 'svelte-sonner';
	import Shapes from '@lucide/svelte/icons/shapes';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import Pencil from '@lucide/svelte/icons/pencil';
	import X from '@lucide/svelte/icons/x';

	let {
		datatypes,
		onAdd
	}: {
		datatypes: DataTypesState;
		/** Called with the DSL term to add, e.g. `datatype:sensor_readings`. */
		onAdd: (term: string) => void;
	} = $props();

	let expanded = $state<Record<string, boolean>>({});
	let dialog = $state<DataTypeDialog>();

	async function remove(dt: CatalogueDataType) {
		if (!confirm(`Delete data type “${dt.name}”? Its schema digests become unregistered again.`))
			return;
		try {
			await datatypes.remove(dt.id);
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'Failed to delete data type');
		}
	}
</script>

<section data-testid="rail-datatypes">
	<div class="mb-2 flex items-center gap-2 text-sm font-medium text-foreground">
		<Shapes class="size-4 text-muted-foreground" />
		Data types
	</div>
	{#if datatypes.loading && datatypes.list.length === 0}
		<p class="px-1 py-1 text-xs text-muted-foreground">Loading…</p>
	{:else if datatypes.list.length === 0}
		<p class="px-1 py-1 text-xs text-muted-foreground">
			No data types yet — expand the Schemas facet and register a schema digest.
		</p>
	{:else}
		<div class="max-h-56 space-y-px overflow-y-auto">
			{#each datatypes.list as dt (dt.id)}
				<div>
					<div class="flex items-center gap-1 rounded px-1 hover:bg-accent">
						<button
							type="button"
							class="shrink-0 rounded p-0.5 text-muted-foreground hover:text-foreground"
							title={expanded[dt.id] ? 'Hide columns' : 'Show columns'}
							onclick={() => (expanded[dt.id] = !expanded[dt.id])}
						>
							{#if expanded[dt.id]}
								<ChevronDown class="size-3.5" />
							{:else}
								<ChevronRight class="size-3.5" />
							{/if}
						</button>
						<button
							type="button"
							class="flex min-w-0 flex-1 items-center gap-2 px-1 py-1 text-left"
							title={`datatype:${dt.name}${dt.description ? ` — ${dt.description}` : ''}`}
							onclick={() => onAdd(`datatype:${quoteIfNeeded(dt.name)}`)}
						>
							<span class="min-w-0 truncate text-sm text-foreground">{dt.name}</span>
							<Badge variant="secondary" class="ml-auto shrink-0 px-1.5 py-0 text-[10px] tabular-nums">
								{dt.entry_count.toLocaleString()}
							</Badge>
						</button>
						<button
							type="button"
							class="shrink-0 rounded p-1 text-muted-foreground hover:text-foreground"
							title="Edit data type"
							onclick={() => dialog?.openEdit(dt)}
						>
							<Pencil class="size-3.5" />
						</button>
						<button
							type="button"
							class="shrink-0 rounded p-1 text-muted-foreground hover:text-destructive"
							title="Delete data type"
							onclick={() => remove(dt)}
						>
							<X class="size-3.5" />
						</button>
					</div>
					{#if expanded[dt.id]}
						<div class="space-y-px pb-1 pl-7 pr-1">
							{#each dt.columns as col (col.name)}
								<div class="flex items-baseline gap-2 px-1 text-xs">
									<span class="min-w-0 truncate font-mono text-foreground">
										{col.name}{#if col.nullable}<span
												class="text-muted-foreground"
												title="nullable">?</span
											>{/if}
									</span>
									<span
										class="ml-auto shrink-0 rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground"
									>
										{col.data_type}
									</span>
								</div>
							{/each}
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</section>

<DataTypeDialog bind:this={dialog} {datatypes} />
