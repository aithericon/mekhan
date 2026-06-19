<script lang="ts">
	// Generic fallback for the catalogue card's "Format & schema" section. This
	// is the reference component for all format-specific metadata renderers:
	// it consumes the normalized FileMetadataView and renders the format chips,
	// dimensions, detail fields, unix mode, schema fingerprint button, column
	// chips, and detail tables. Format-specific renderers may replace it for
	// richer presentation; everything else degrades to this.
	import type { MetadataProps } from './types';
	import { pct, symbolicMode } from './utils';
	import DetailTable from '../DetailTable.svelte';

	let { mv, onSchemaClick }: MetadataProps = $props();

	const formatLabel = $derived(mv.format ?? null);
	const schema = $derived(mv.schema_fingerprint ?? null);
	const columns = $derived(mv.columns ?? []);
	const details = $derived(mv.details ?? null);
	const numRows = $derived(mv.num_rows ?? null);
	const numCols = $derived(mv.num_columns ?? null);
	const unixMode = $derived(mv.unix_mode ?? null);
	// Dims that duplicate a chip shown elsewhere (rows×cols, image width/height in
	// details) add noise; surface only the interesting ones (z/y/x, lat/lon, …).
	const REDUNDANT_DIMS = new Set(['rows', 'columns', 'width', 'height']);
	const dims = $derived((mv.dimensions ?? []).filter((d) => !REDUNDANT_DIMS.has(d.name)));
</script>

<!-- Format & schema -->
{#if formatLabel || schema || details || numRows != null || columns.length > 0 || dims.length > 0 || unixMode != null}
	<section>
		<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Format &amp; schema</h4>
		<div class="flex flex-wrap items-center gap-x-2 gap-y-1.5">
			{#if formatLabel}
				<span class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs">{formatLabel}</span>
			{/if}
			{#if numRows != null}
				<span class="rounded border border-border bg-background px-1.5 py-0.5 text-xs tabular-nums">{numRows.toLocaleString()} rows × {numCols ?? '?'} cols</span>
			{/if}
			{#each dims as d}
				<span class="rounded border border-border bg-background px-1.5 py-0.5 text-xs">
					<span class="text-muted-foreground">{d.name}:</span>
					<span class="font-medium text-foreground tabular-nums">{d.size != null ? d.size.toLocaleString() : '∞'}</span>
				</span>
			{/each}
			{#if details}
				{#each details.fields ?? [] as f}
					<span class="rounded border border-border bg-background px-1.5 py-0.5 text-xs">
						<span class="text-muted-foreground">{f.label}:</span>
						<span class="font-medium text-foreground">{f.value}{f.unit ? ` ${f.unit}` : ''}</span>
					</span>
				{/each}
			{/if}
			{#if unixMode != null}
				<span class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs" title="unix mode {unixMode}">{symbolicMode(unixMode)}</span>
			{/if}
			{#if schema?.digest}
				<button
					class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs hover:border-primary hover:text-primary"
					onclick={() => onSchemaClick?.(schema!.digest)}
					title="Filter by this schema fingerprint (v{schema.version})"
				>schema {schema.digest}</button>
			{/if}
		</div>

		{#if columns.length > 0}
			<div class="mt-2 flex flex-wrap gap-1">
				{#each columns as col}
					<span class="inline-flex items-center gap-1 rounded border border-border bg-background px-1.5 py-0.5 text-xs">
						<span class="font-medium text-foreground">{col.name}</span>
						<span class="text-muted-foreground">{col.data_type}{col.nullable ? '?' : ''}</span>
						{#each col.classifications ?? [] as tag}
							<span class="rounded-sm bg-amber-500/10 px-1 text-[10px] text-amber-600 dark:text-amber-400" title="{pct(tag.confidence)} confidence">{tag.category}</span>
						{/each}
					</span>
				{/each}
			</div>
		{/if}

		{#if details}
			{#each details.tables ?? [] as t}
				<DetailTable title={t.title} columns={t.columns} rows={t.rows} />
			{/each}
		{/if}
	</section>
{/if}
