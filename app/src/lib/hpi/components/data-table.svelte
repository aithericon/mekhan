<script lang="ts">
	let { headers, rows, alignments, caption, renderCell }: {
		headers: string[];
		rows: string[][];
		alignments?: ('left' | 'center' | 'right')[];
		caption?: string;
		renderCell?: (text: string) => string;
	} = $props();

	function cellHtml(text: string): string {
		return renderCell ? renderCell(text) : text;
	}
</script>

<div class="overflow-hidden rounded-xl border border-border bg-card/70 shadow-sm">
	{#if caption}
		<p class="pt-3 pb-2 text-center text-base text-muted-foreground">{caption}</p>
	{/if}
	<div class="overflow-x-auto">
		<table class="w-full border-collapse">
			<thead>
				<tr class="border-t border-b border-border">
					{#each headers as header, i (i)}
						<th class="border-r border-border px-4 py-2 text-left text-base font-semibold text-foreground last:border-r-0"
							style="text-align: {alignments?.[i] ?? 'left'}">
							<!-- eslint-disable-next-line svelte/no-at-html-tags -->
							{@html cellHtml(header)}
						</th>
					{/each}
				</tr>
			</thead>
			<tbody>
				{#each rows as row, rowIdx (rowIdx)}
					<tr class="{rowIdx % 2 === 1 ? 'bg-muted/20' : ''} border-b border-border last:border-b-0">
						{#each row as cell, i (i)}
							<td class="border-r border-border px-4 py-2 text-base text-foreground last:border-r-0"
								style="text-align: {alignments?.[i] ?? 'left'}">
								<!-- eslint-disable-next-line svelte/no-at-html-tags -->
								{@html cellHtml(cell)}
							</td>
						{/each}
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
</div>
