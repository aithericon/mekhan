<script lang="ts">
	import PrimitiveValue from './PrimitiveValue.svelte';
	import FileReference from './FileReference.svelte';
	import type { RendererProps } from './types';

	let { value, ctx }: RendererProps = $props();

	type Entry = [string, unknown];

	const entries = $derived<Entry[]>(
		value && typeof value === 'object' && !Array.isArray(value)
			? Object.entries(value as Record<string, unknown>)
			: []
	);

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
			return s.length > 80 ? s.slice(0, 77) + '…' : s;
		} catch {
			return String(v);
		}
	}
</script>

<dl class="grid grid-cols-[minmax(8rem,max-content)_1fr] gap-x-4 gap-y-2 rounded-md border border-border bg-muted/20 p-3 text-sm">
	{#each entries as [key, v] (key)}
		<dt class="font-mono text-sm text-muted-foreground" title={key}>{key}</dt>
		<dd class="min-w-0 break-words">
			{#if isPrimitive(v)}
				<PrimitiveValue value={v} {ctx} />
			{:else if isFileRef(v)}
				<FileReference value={v} {ctx} />
			{:else}
				<code class="rounded bg-muted px-1.5 py-0.5 font-mono text-sm text-muted-foreground">
					{compactJson(v)}
				</code>
			{/if}
		</dd>
	{/each}
</dl>
