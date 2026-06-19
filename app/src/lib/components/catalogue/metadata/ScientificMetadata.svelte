<script lang="ts">
	// Format-specific metadata renderer for the SCIENTIFIC and MESH families
	// (hdf5, net_cdf, fits, zarr_v2/v3, vtk_legacy/vtu/vtp/vts/vtr/vti). These
	// formats are about STRUCTURE — groups, variables, dimensions, data arrays —
	// so we lean on dimensions chips, a variables/datasets table, detail-field
	// chips, and the rich detail tables (hdf5 groups, zarr hierarchy, fits
	// header cards, vtk point/cell data). Mirrors GenericFormatMetadata's look.
	import type { MetadataProps } from './types';
	import DetailTable from '../DetailTable.svelte';

	let { mv, onSchemaClick }: MetadataProps = $props();

	const formatLabel = $derived(mv.format ?? null);
	const schema = $derived(mv.schema_fingerprint ?? null);
	// Variables/datasets, not tabular columns — show every one.
	const variables = $derived(mv.columns ?? []);
	const details = $derived(mv.details ?? null);
	// Dimensions are first-class here (time/lat/lon/depth/x/y/z): show ALL, no
	// rows/columns/width/height filter (those are tabular concepts).
	const dims = $derived(mv.dimensions ?? []);
</script>

<!-- Format & schema -->
{#if formatLabel || schema || details || variables.length > 0 || dims.length > 0}
	<section>
		<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Format &amp; schema</h4>
		<div class="flex flex-wrap items-center gap-x-2 gap-y-1.5">
			{#if formatLabel}
				<span class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs">{formatLabel}</span>
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
			{#if schema?.digest}
				<button
					class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs hover:border-primary hover:text-primary"
					onclick={() => onSchemaClick?.(schema!.digest)}
					title="Filter by this schema fingerprint (v{schema.version})"
				>schema {schema.digest}</button>
			{/if}
		</div>

		{#if variables.length > 0}
			<DetailTable
				title="variables"
				columns={['name', 'data type']}
				rows={variables.map((v) => [v.name, `${v.data_type}${v.nullable ? '?' : ''}`])}
			/>
		{/if}

		{#if details}
			{#each details.tables ?? [] as t}
				<DetailTable title={t.title} columns={t.columns} rows={t.rows} />
			{/each}
		{/if}
	</section>
{/if}
