<script lang="ts">
	// Render the Postgres backend's result envelope — the `{rows, row_count,
	// rows_affected}` shape `executor-postgres` produces.
	//
	//  - `rows`: array of row-objects (columns drawn from the row keys; on a
	//    read this matches the declared `projection`).
	//  - `row_count`: number of rows returned (present on read; on write it's
	//    the count of RETURNING rows, if any).
	//  - `rows_affected`: command-tag count for write (null on read).
	//
	// Predicate registration lives in `./index.ts::matchesPostgresResult`.
	// Mirrors `SmtpEnvelope.svelte`'s structure + styling.

	import { Badge } from '$lib/components/ui/badge';
	import Database from '@lucide/svelte/icons/database';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import type { RendererProps } from './types';

	type PostgresResult = {
		rows?: unknown;
		row_count?: number | null;
		rows_affected?: number | null;
	};

	let { value }: RendererProps = $props();
	const result = $derived(value as PostgresResult);

	const rows = $derived(Array.isArray(result.rows) ? (result.rows as unknown[]) : []);
	const rowCount = $derived(typeof result.row_count === 'number' ? result.row_count : null);
	const rowsAffected = $derived(
		typeof result.rows_affected === 'number' ? result.rows_affected : null
	);

	// Union the column keys across the (object) rows so heterogeneous rows
	// still render every column. Non-object rows are tolerated — they fall
	// back to a single "value" column.
	function isObj(v: unknown): v is Record<string, unknown> {
		return !!v && typeof v === 'object' && !Array.isArray(v);
	}

	const columns = $derived.by(() => {
		const seen: string[] = [];
		const set = new Set<string>();
		for (const r of rows) {
			if (!isObj(r)) continue;
			for (const k of Object.keys(r)) {
				if (!set.has(k)) {
					set.add(k);
					seen.push(k);
				}
			}
		}
		return seen;
	});

	const allObjects = $derived(rows.length > 0 && rows.every(isObj));

	function cellText(v: unknown): string {
		if (v === null || v === undefined) return '';
		if (typeof v === 'object') return JSON.stringify(v);
		return String(v);
	}

	let showRaw = $state(false);
</script>

<div class="space-y-3 rounded-lg border border-border bg-card p-3">
	<header class="flex items-start gap-2">
		<Database class="mt-0.5 size-5 shrink-0 text-muted-foreground" />
		<div class="min-w-0 flex-1">
			<div class="flex flex-wrap items-center gap-2">
				{#if rowCount !== null}
					<Badge variant="secondary" class="gap-1" data-testid="pg-row-count">
						{rowCount}
						{rowCount === 1 ? 'row' : 'rows'}
					</Badge>
				{/if}
				{#if rowsAffected !== null}
					<Badge variant="default" class="gap-1" data-testid="pg-rows-affected">
						{rowsAffected} affected
					</Badge>
				{/if}
			</div>
		</div>
	</header>

	{#if rows.length > 0 && allObjects && columns.length > 0}
		<div class="overflow-auto rounded border border-border">
			<table class="w-full border-collapse text-sm">
				<thead>
					<tr class="border-b border-border bg-muted/40">
						{#each columns as col (col)}
							<th class="px-2 py-1 text-left font-medium text-muted-foreground">{col}</th>
						{/each}
					</tr>
				</thead>
				<tbody>
					{#each rows as row, ri (ri)}
						<tr class="border-b border-border/50 last:border-0">
							{#each columns as col (col)}
								<td class="px-2 py-1 align-top font-mono text-sm"
									>{cellText(isObj(row) ? row[col] : undefined)}</td
								>
							{/each}
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{:else if rows.length > 0}
		<!-- Rows present but not uniform objects: list each row as JSON. -->
		<div class="space-y-1">
			{#each rows as row, ri (ri)}
				<pre
					class="overflow-auto whitespace-pre-wrap rounded border border-border bg-muted/40 p-2 font-mono text-sm">{cellText(
						row
					)}</pre>
			{/each}
		</div>
	{:else}
		<p class="text-sm italic text-muted-foreground">No rows returned.</p>
	{/if}

	<div>
		<button
			type="button"
			class="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
			onclick={() => (showRaw = !showRaw)}
		>
			{#if showRaw}
				<ChevronDown class="size-3" />
				Hide raw result
			{:else}
				<ChevronRight class="size-3" />
				Show raw result
			{/if}
		</button>
		{#if showRaw}
			<pre
				class="mt-1 max-h-48 overflow-auto whitespace-pre rounded border border-border bg-muted/40 p-2 font-mono text-sm">{JSON.stringify(
					result,
					null,
					2
				)}</pre>
		{/if}
	</div>
</div>
