<script lang="ts">
	// Rich "Format & schema" renderer for the TABULAR and SPREADSHEET families
	// (csv, parquet, arrow, json, xlsx, xls, ods). Mirrors GenericFormatMetadata's
	// look & feel, but renders columns as a proper scrollable table instead of
	// wrapping chips — wide schemas (10+ columns) are unreadable as chips.
	import type { MetadataProps } from './types';
	import { pct } from './utils';
	import DetailTable from '../DetailTable.svelte';

	let { mv, onSchemaClick }: MetadataProps = $props();

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
		<div class="mt-2 overflow-hidden rounded-md border border-border bg-background">
			<div class="overflow-x-auto">
				<table class="w-full text-xs">
					<thead>
						<tr class="border-b border-border text-left text-muted-foreground">
							<th class="whitespace-nowrap px-2 py-1 font-medium capitalize">Column</th>
							<th class="whitespace-nowrap px-2 py-1 font-medium capitalize">Type</th>
							<th class="whitespace-nowrap px-2 py-1 font-medium capitalize">Nullable</th>
							<th class="whitespace-nowrap px-2 py-1 font-medium capitalize">Tags</th>
						</tr>
					</thead>
					<tbody>
						{#each columns as col}
							<tr class="border-b border-border/40 last:border-0">
								<td class="whitespace-nowrap px-2 py-1 font-medium text-foreground" title={col.name}>{col.name}</td>
								<td class="whitespace-nowrap px-2 py-1 text-muted-foreground">{col.data_type}</td>
								<td class="whitespace-nowrap px-2 py-1 text-muted-foreground">{col.nullable ? 'nullable' : '—'}</td>
								<td class="px-2 py-1">
									{#if (col.classifications ?? []).length > 0}
										<span class="flex flex-wrap gap-1">
											{#each col.classifications ?? [] as tag}
												<span class="rounded-sm bg-amber-500/10 px-1 text-[10px] text-amber-600 dark:text-amber-400" title="{pct(tag.confidence)} confidence">{tag.category}</span>
											{/each}
										</span>
									{:else}
										<span class="text-muted-foreground">—</span>
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
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
