<script lang="ts">
	// A compact, horizontally-scrollable table. Used for normalized format
	// `details.tables` (Parquet row groups, archive entries, …) and tabular
	// previews. Cells arrive pre-stringified from the backend `FileMetadataView`,
	// so there is no client-side formatting to drift.
	let {
		title = '',
		columns,
		rows,
		max = 12
	}: { title?: string; columns: string[]; rows: string[][]; max?: number } = $props();

	const shown = $derived(rows.slice(0, max));
</script>

<div class="mt-2 overflow-hidden rounded-md border border-border bg-background">
	{#if title}
		<div class="border-b border-border px-2 py-1 text-[11px] font-medium capitalize text-muted-foreground">
			{title}
		</div>
	{/if}
	<div class="overflow-x-auto">
		<table class="w-full text-xs">
			<thead>
				<tr class="border-b border-border text-left text-muted-foreground">
					{#each columns as c}
						<th class="whitespace-nowrap px-2 py-1 font-medium capitalize">{c}</th>
					{/each}
				</tr>
			</thead>
			<tbody>
				{#each shown as row}
					<tr class="border-b border-border/40 last:border-0">
						{#each row as cell}
							<td class="max-w-[18rem] truncate px-2 py-1 font-mono text-foreground" title={cell}>{cell}</td>
						{/each}
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
	{#if rows.length > max}
		<div class="border-t border-border px-2 py-1 text-[11px] text-muted-foreground">
			+{rows.length - max} more {rows.length - max === 1 ? 'row' : 'rows'}
		</div>
	{/if}
</div>
