<script lang="ts">
	import PrimitiveValue from './PrimitiveValue.svelte';
	import FileReference from './FileReference.svelte';
	import type { RendererProps } from './types';

	let { value, ctx }: RendererProps = $props();

	const rows = $derived<Record<string, unknown>[]>(
		Array.isArray(value)
			? (value as unknown[]).filter(
					(r): r is Record<string, unknown> =>
						!!r && typeof r === 'object' && !Array.isArray(r)
				)
			: []
	);

	// Union of keys, preserving first-seen order so columns line up with the
	// shape the producer emitted rather than alphabetised noise.
	const columns = $derived.by<string[]>(() => {
		const seen = new Set<string>();
		const out: string[] = [];
		for (const r of rows) {
			for (const k of Object.keys(r)) {
				if (!seen.has(k)) {
					seen.add(k);
					out.push(k);
				}
			}
		}
		return out;
	});

	function isPrimitive(v: unknown): boolean {
		return v === null || v === undefined || typeof v !== 'object';
	}

	function isFileRef(v: unknown): boolean {
		return (
			!!v &&
			typeof v === 'object' &&
			!Array.isArray(v) &&
			typeof (v as Record<string, unknown>).url === 'string'
		);
	}

	function compactJson(v: unknown): string {
		try {
			const s = JSON.stringify(v);
			return s.length > 40 ? s.slice(0, 37) + '…' : s;
		} catch {
			return String(v);
		}
	}
</script>

<div class="overflow-hidden rounded-md border border-border">
	<div class="border-b border-border bg-muted/30 px-3 py-1.5 text-sm text-muted-foreground">
		{rows.length} row{rows.length === 1 ? '' : 's'}
	</div>
	<div class="overflow-x-auto">
		<table class="w-full text-sm">
			<thead class="bg-muted/20 text-sm uppercase tracking-wide text-muted-foreground">
				<tr>
					{#each columns as col (col)}
						<th class="px-3 py-2 text-left font-medium font-mono">{col}</th>
					{/each}
				</tr>
			</thead>
			<tbody>
				{#each rows as row, i (i)}
					<tr class="border-t border-border align-top">
						{#each columns as col (col)}
							{@const cell = row[col]}
							<td class="px-3 py-2 break-words">
								{#if cell === undefined}
									<span class="text-sm text-muted-foreground/60">—</span>
								{:else if isPrimitive(cell)}
									<PrimitiveValue value={cell} {ctx} />
								{:else if isFileRef(cell)}
									<FileReference value={cell} {ctx} />
								{:else}
									<code class="rounded bg-muted px-1.5 py-0.5 font-mono text-sm text-muted-foreground">{compactJson(cell)}</code>
								{/if}
							</td>
						{/each}
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
</div>
