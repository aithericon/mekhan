<script lang="ts">
	// Archive-family metadata renderer (zip, tar, gzip, bzip2, xz, zstd,
	// seven_zip, rar). Surfaces the archive summary (entry count, compressed vs
	// uncompressed totals, compression, encryption, comment) as a labeled spec
	// grid, then the per-entry listing via the scrollable DetailTable. Falls back
	// to nothing when there are no details.
	import type { MetadataProps } from './types';
	import DetailTable from '../DetailTable.svelte';

	let { mv, onSchemaClick }: MetadataProps = $props();

	const formatLabel = $derived(mv.format ?? null);
	const details = $derived(mv.details ?? null);
	const fields = $derived(details?.fields ?? []);
	const tables = $derived(details?.tables ?? []);

	// The per-entry listing, if the backend emitted one.
	const entriesTable = $derived(tables.find((t) => t.title === 'entries') ?? null);
	// Any other tables (rare for archives) render generically.
	const otherTables = $derived(tables.filter((t) => t.title !== 'entries'));

	// "num entries" is the true total; the entries table is capped server-side,
	// so note the discrepancy when the listing is partial.
	const numEntries = $derived.by(() => {
		const f = fields.find((x) => x.label === 'num entries');
		if (!f) return null;
		const n = Number(String(f.value).replace(/[, ]/g, ''));
		return Number.isFinite(n) ? n : null;
	});
	const shownEntries = $derived(entriesTable?.rows.length ?? 0);
	const partial = $derived(numEntries != null && numEntries > shownEntries);
</script>

<!-- Archive contents -->
{#if details}
	<section>
		<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Archive contents</h4>

		<div class="flex flex-wrap items-center gap-x-2 gap-y-1.5">
			{#if formatLabel}
				<span class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs">{formatLabel}</span>
			{/if}
			{#each fields as f}
				<span class="rounded border border-border bg-background px-1.5 py-0.5 text-xs">
					<span class="text-muted-foreground">{f.label}:</span>
					<span class="font-medium text-foreground">{f.value}{f.unit ? ` ${f.unit}` : ''}</span>
				</span>
			{/each}
		</div>

		{#if entriesTable}
			<DetailTable title={entriesTable.title} columns={entriesTable.columns} rows={entriesTable.rows} />
			{#if partial}
				<div class="mt-1 text-[11px] text-muted-foreground">
					showing {shownEntries.toLocaleString()} of {numEntries!.toLocaleString()} entries
				</div>
			{/if}
		{/if}

		{#each otherTables as t}
			<DetailTable title={t.title} columns={t.columns} rows={t.rows} />
		{/each}
	</section>
{/if}
