<script lang="ts">
	// Rich "Format & schema" renderer for the TABULAR and SPREADSHEET families
	// (csv, parquet, arrow, json, xlsx, xls, ods). Mirrors GenericFormatMetadata's
	// look & feel, but renders columns as a proper scrollable table instead of
	// wrapping chips — wide schemas (10+ columns) are unreadable as chips.
	import type { MetadataProps } from './types';
	import { pct } from './utils';
	import DetailTable from '../DetailTable.svelte';
	import SchemaView from '$lib/schema/SchemaView.svelte';
	import { fileMetadataDataTypeToSchemaNode } from '$lib/schema/model';

	let { mv, columnSchemas, onSchemaClick }: MetadataProps = $props();

	// The type node for a column: the raw nested schema (struct/list capable) when
	// the probe's columns were threaded through, else parse the humanized scalar
	// string (`string`, `int64`, …) so even legacy rows render a consistent badge.
	function nodeFor(col: { name: string; data_type: string }) {
		return columnSchemas?.get(col.name) ?? fileMetadataDataTypeToSchemaNode(col.data_type);
	}

	const formatLabel = $derived(mv.format ?? null);
	const schema = $derived(mv.schema_fingerprint ?? null);
	const columns = $derived(mv.columns ?? []);
	const details = $derived(mv.details ?? null);
	const numRows = $derived(mv.num_rows ?? null);
	const numCols = $derived(mv.num_columns ?? null);
</script>

<!-- Format & schema -->
<section>
	<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Format &amp; schema</h4>

	<div class="flex flex-wrap items-center gap-x-2 gap-y-1.5">
		{#if formatLabel}
			<span class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs">{formatLabel}</span>
		{/if}
		{#if numRows != null}
			<span class="rounded border border-border bg-background px-1.5 py-0.5 text-xs tabular-nums">{numRows.toLocaleString()} rows × {numCols != null ? numCols.toLocaleString() : '?'} cols</span>
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
		<!-- Schema as a tree (our preferred representation): each column is a field
		     row rendered by the shared SchemaView, with nullable / classification
		     chips inline on the same line rather than as separate table columns. -->
		<div class="mt-2 overflow-hidden rounded-md border border-border bg-background text-sm">
			{#each columns as col}
				<div class="border-b border-border/40 px-2 py-1.5 last:border-0">
					<SchemaView node={nodeFor(col)} label={col.name}>
						{#snippet trailing()}
							<span class="ml-auto flex items-center gap-1.5 pl-2">
								{#each col.classifications ?? [] as tag}
									<span
										class="rounded-sm bg-amber-500/10 px-1 text-[10px] text-amber-600 dark:text-amber-400"
										title="{pct(tag.confidence)} confidence">{tag.category}</span
									>
								{/each}
								{#if col.nullable}
									<span class="text-[10px] text-muted-foreground/50">nullable</span>
								{/if}
							</span>
						{/snippet}
					</SchemaView>
				</div>
			{/each}
		</div>
	{/if}

	{#if details && (details.fields ?? []).length > 0}
		<div class="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1.5">
			{#each details.fields ?? [] as f}
				<span class="rounded border border-border bg-background px-1.5 py-0.5 text-xs">
					<span class="text-muted-foreground">{f.label}:</span>
					<span class="font-medium text-foreground">{f.value}{f.unit ? ` ${f.unit}` : ''}</span>
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
