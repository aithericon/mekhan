<script lang="ts">
	// Structured-text metadata renderer for the DOCUMENT family (markdown, xml,
	// html, txt) and CONFIG family (toml, yaml, ini, env). Renders the format
	// chip + optional schema button, the detail fields as a labeled spec grid,
	// and any detail tables. Markdown headings get a special-cased indented
	// outline instead of a flat table; all other tables (xml namespaces, html
	// meta tags, …) render via the scrollable DetailTable.
	import type { MetadataProps } from './types';
	import DetailTable from '../DetailTable.svelte';

	let { mv, onSchemaClick }: MetadataProps = $props();

	const formatLabel = $derived(mv.format ?? null);
	const schema = $derived(mv.schema_fingerprint ?? null);
	const details = $derived(mv.details ?? null);
	const fields = $derived(details?.fields ?? []);
	const tables = $derived(details?.tables ?? []);

	// Markdown emits a "headings" table (columns: level, text) which we render as
	// an outline; everything else is a plain DetailTable.
	const headingsTable = $derived(tables.find((t) => t.title === 'headings') ?? null);
	const otherTables = $derived(tables.filter((t) => t !== headingsTable));

	// Resolve which row column holds the heading level and which the text, so an
	// outline indents correctly regardless of column order.
	const headingOutline = $derived.by(() => {
		if (!headingsTable) return [];
		const cols = headingsTable.columns.map((c) => c.toLowerCase());
		const levelIdx = cols.indexOf('level');
		const textIdx = cols.indexOf('text');
		return headingsTable.rows.map((row) => {
			const rawLevel = levelIdx >= 0 ? row[levelIdx] : '1';
			const level = Math.max(1, Math.min(6, Number(rawLevel) || 1));
			const text = textIdx >= 0 ? row[textIdx] : row.join(' ');
			return { level, text };
		});
	});

	const hasContent = $derived(
		!!formatLabel || !!schema?.digest || fields.length > 0 || tables.length > 0
	);
</script>

<!-- Format & schema -->
{#if hasContent}
	<section>
		<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Format &amp; schema</h4>

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
			{#if schema?.digest}
				<button
					class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs hover:border-primary hover:text-primary"
					onclick={() => onSchemaClick?.(schema!.digest)}
					title="Filter by this schema fingerprint (v{schema.version})"
				>schema {schema.digest}</button>
			{/if}
		</div>

		{#if headingOutline.length > 0}
			<div class="mt-2 overflow-hidden rounded-md border border-border bg-background">
				<div class="border-b border-border px-2 py-1 text-[11px] font-medium capitalize text-muted-foreground">
					headings
				</div>
				<div class="py-1">
					{#each headingOutline as h}
						<div
							class="truncate px-2 py-0.5 text-xs text-foreground"
							style="padding-left: {0.5 + (h.level - 1) * 0.875}rem"
							title={h.text}
						>
							<span class="text-muted-foreground">{'#'.repeat(h.level)}</span>
							<span class="ml-1 font-medium">{h.text}</span>
						</div>
					{/each}
				</div>
			</div>
		{/if}

		{#each otherTables as t}
			<DetailTable title={t.title} columns={t.columns} rows={t.rows} />
		{/each}
	</section>
{/if}
