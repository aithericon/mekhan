<script lang="ts">
	// The Entries query rail — the persistent control surface beside the
	// result list. Querying is an iterative loop, so the pieces that used to
	// hide in QueryBar popovers live here permanently: query-scoped facet
	// groups, saved queries, the field reference (inserts into the QueryBar
	// draft), and the syntax cheat-sheet.
	import {
		getCatalogueQueryFields,
		listSavedQueries,
		createSavedQuery,
		deleteSavedQuery,
		type QueryFieldsResponse,
		type SavedQuery
	} from '$lib/api/data';
	import { ApiError } from '$lib/api/client';
	import { parseQuery, activeFormats } from './query-language';
	import type { EntriesQueryState } from './entries-query.svelte';
	import type { DataTypesState } from './data-types.svelte';
	import FacetGroup from './FacetGroup.svelte';
	import SchemaFacetGroup from './SchemaFacetGroup.svelte';
	import DataTypesSection from './DataTypesSection.svelte';
	import FieldReference from './FieldReference.svelte';
	import { SideRail } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { toast } from 'svelte-sonner';
	import ListFilter from '@lucide/svelte/icons/list-filter';
	import Bookmark from '@lucide/svelte/icons/bookmark';
	import CircleHelp from '@lucide/svelte/icons/circle-help';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import X from '@lucide/svelte/icons/x';

	let { entries, datatypes }: { entries: EntriesQueryState; datatypes: DataTypesState } =
		$props();

	// Heavier dimensions (column / classification → LATERAL jsonb unnests)
	// start collapsed; FacetGroup only fetches expanded groups.
	const DIMENSIONS = [
		{ dim: 'format', label: 'Format', termPrefix: 'format', defaultExpanded: true },
		{ dim: 'category', label: 'Category', termPrefix: 'category', defaultExpanded: true },
		{ dim: 'source_net', label: 'Net', termPrefix: 'source_net', defaultExpanded: false },
		{ dim: 'column', label: 'Column', termPrefix: 'col', defaultExpanded: false },
		{ dim: 'classification', label: 'PII', termPrefix: 'pii', defaultExpanded: false }
	];

	// ── Saved queries (relocated from the QueryBar popover) ──────────────────
	let saved = $state<SavedQuery[]>([]);
	let savedLoading = $state(true);
	let saveName = $state('');
	let saving = $state(false);

	async function loadSaved() {
		savedLoading = true;
		try {
			saved = await listSavedQueries();
		} catch {
			saved = [];
		} finally {
			savedLoading = false;
		}
	}
	$effect(() => {
		loadSaved();
	});

	async function saveCurrent() {
		const name = saveName.trim();
		if (!name) return;
		saving = true;
		try {
			const q = entries.applied;
			// Persist only the raw DSL text — replay goes through `entries.apply(sq.q)`,
			// which now compiles server-side (single compiler). The legacy `params`
			// snapshot is no longer request-driving, so it stays empty.
			await createSavedQuery({ name, q, params: {} });
			toast.success(`Saved query “${name}”`);
			saveName = '';
			await loadSaved();
		} catch (e) {
			if (e instanceof ApiError && e.status === 409) {
				toast.error(`A saved query named “${name}” already exists`);
			} else {
				toast.error(e instanceof Error ? e.message : 'Failed to save query');
			}
		} finally {
			saving = false;
		}
	}

	async function removeSaved(sq: SavedQuery) {
		if (!confirm(`Delete saved query “${sq.name}”?`)) return;
		try {
			await deleteSavedQuery(sq.id);
			saved = saved.filter((s) => s.id !== sq.id);
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'Failed to delete saved query');
		}
	}

	// ── Field reference (static per server build — module-cached fetch) ──────
	let registry = $state<QueryFieldsResponse | null>(null);
	$effect(() => {
		getCatalogueQueryFields()
			.then((r) => (registry = r))
			.catch(() => {});
	});
	let helpOpen = $state(false);

	// Formats asserted by the APPLIED query (not the draft — facets already
	// scope to applied; don't reshuffle the reference per keystroke).
	const appliedFormats = $derived(activeFormats(parseQuery(entries.applied).terms));

	function insertField(term: string) {
		entries.insertDraft(term);
		// Land the cursor in the bar so the user can complete the stub.
		queueMicrotask(() =>
			document.querySelector<HTMLInputElement>('[data-testid="query-bar-input"]')?.focus()
		);
	}

	// Static syntax cheat-sheet.
	const HELP_ROWS: Array<[string, string]> = [
		['word · "free text"', 'free-text search over name / hash'],
		['field:value', 'equals · field!=value for not-equals'],
		['field:a,b,c', 'any of (unquoted comma list)'],
		['filename~rep · ^run- · $.csv', 'substring · starts-with · ends-with'],
		['field:null · field:*', 'missing · present'],
		['size_bytes>10m', 'comparisons > >= < <= · byte suffixes k/m/g/t'],
		['created_at>-7d', 'relative dates m/h/d/w/y · or ISO dates'],
		['format:csv', 'file_metadata format'],
		['meta.delimiter:";"', 'format detail fields use flat meta.* names'],
		['datatype:name', 'entries of a registered data type'],
		['col:email · dim:time', 'has column · has dimension'],
		['pii:EMAIL', 'has a column classified as…'],
		['attr:KEY=VALUE', 'custom attribute'],
		['umeta.kind:value', 'match a user_metadata key (any key)'],
		['owner:"null"', 'quoting opts out of special forms']
	];
</script>

<SideRail testid="data-query-rail">
	<div class="space-y-6 p-4">
		<!-- Facets -->
		<section>
			<div class="mb-2 flex items-center gap-2 text-sm font-medium text-foreground">
				<ListFilter class="size-4 text-muted-foreground" />
				Facets
			</div>
			<div class="space-y-2">
				{#each DIMENSIONS as d (d.dim)}
					<FacetGroup
						dim={d.dim}
						label={d.label}
						termPrefix={d.termPrefix}
						defaultExpanded={d.defaultExpanded}
						query={entries.applied}
						onAdd={(term) => entries.addTerm(term)}
					/>
				{/each}
				<SchemaFacetGroup
					query={entries.applied}
					{datatypes}
					onAdd={(term) => entries.addTerm(term)}
				/>
			</div>
		</section>

		<!-- Data types (registered schema digests) -->
		<DataTypesSection {datatypes} onAdd={(term) => entries.addTerm(term)} />

		<!-- Saved queries -->
		<section data-testid="rail-saved">
			<div class="mb-2 flex items-center gap-2 text-sm font-medium text-foreground">
				<Bookmark class="size-4 text-muted-foreground" />
				Saved queries
			</div>
			<div class="max-h-56 space-y-px overflow-y-auto">
				{#if savedLoading}
					<p class="px-1 py-1 text-xs text-muted-foreground">Loading…</p>
				{:else if saved.length === 0}
					<p class="px-1 py-1 text-xs text-muted-foreground">No saved queries yet</p>
				{:else}
					{#each saved as sq (sq.id)}
						<div
							class={`flex items-center gap-1 rounded px-1 hover:bg-accent ${sq.q === entries.applied ? 'bg-accent' : ''}`}
						>
							<button
								type="button"
								class="min-w-0 flex-1 px-1 py-1 text-left"
								title={sq.q}
								onclick={() => entries.apply(sq.q)}
							>
								<span class="block truncate text-sm text-foreground">{sq.name}</span>
								<span class="block truncate font-mono text-xs text-muted-foreground">{sq.q}</span>
							</button>
							<button
								type="button"
								class="rounded p-1 text-muted-foreground hover:text-destructive"
								title="Delete saved query"
								onclick={() => removeSaved(sq)}
							>
								<X class="size-3.5" />
							</button>
						</div>
					{/each}
				{/if}
			</div>
			<div class="mt-2 flex items-center gap-2">
				<Input
					type="text"
					placeholder="Save current as…"
					class="h-8 flex-1 text-sm"
					bind:value={saveName}
					onkeydown={(e) => {
						if (e.key === 'Enter') {
							e.preventDefault();
							saveCurrent();
						}
					}}
					data-testid="rail-save-name"
				/>
				<Button
					variant="secondary"
					size="sm"
					class="h-8"
					disabled={saving || !saveName.trim() || !entries.applied.trim()}
					onclick={saveCurrent}
					data-testid="rail-save"
				>
					Save
				</Button>
			</div>
		</section>

		<!-- Field reference (narrows to the applied query's formats) -->
		<FieldReference {registry} activeFormats={appliedFormats} onInsert={insertField} />

		<!-- Syntax cheat-sheet -->
		<section data-testid="rail-help">
			<button
				type="button"
				class="flex w-full items-center gap-2 text-sm font-medium text-foreground"
				onclick={() => (helpOpen = !helpOpen)}
			>
				{#if helpOpen}
					<ChevronDown class="size-4 text-muted-foreground" />
				{:else}
					<ChevronRight class="size-4 text-muted-foreground" />
				{/if}
				<CircleHelp class="size-4 text-muted-foreground" />
				Syntax
			</button>
			{#if helpOpen}
				<table class="mt-2 w-full text-sm">
					<tbody>
						{#each HELP_ROWS as [syntax, meaning] (syntax)}
							<tr class="align-baseline">
								<td class="whitespace-nowrap pr-2 font-mono text-xs text-foreground">{syntax}</td>
								<td class="py-0.5 text-xs text-muted-foreground">{meaning}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
		</section>
	</div>
</SideRail>
