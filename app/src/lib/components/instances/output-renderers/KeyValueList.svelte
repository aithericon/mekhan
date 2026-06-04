<script lang="ts">
	import PrimitiveValue from './PrimitiveValue.svelte';
	import FileReference from './FileReference.svelte';
	import StorageRefValue from './StorageRefValue.svelte';
	import SchemaValueView from '$lib/schema/SchemaValueView.svelte';
	import { isPrimitive, isFileRef, isStorageKey } from '$lib/schema/model';
	import type { RendererProps } from './types';

	let { value, ctx }: RendererProps = $props();

	type Entry = [string, unknown];

	const entries = $derived<Entry[]>(
		value && typeof value === 'object' && !Array.isArray(value)
			? Object.entries(value as Record<string, unknown>)
			: []
	);
</script>

<dl class="grid grid-cols-[minmax(8rem,max-content)_1fr] gap-x-4 gap-y-2 rounded-md border border-border bg-muted/20 p-3 text-sm">
	{#each entries as [key, v] (key)}
		<dt class="font-mono text-sm text-muted-foreground" title={key}>{key}</dt>
		<dd class="min-w-0 break-words">
			{#if isStorageKey(v)}
				<StorageRefValue value={v} {ctx} />
			{:else if isPrimitive(v)}
				<PrimitiveValue value={v} {ctx} />
			{:else if isFileRef(v)}
				<FileReference value={v} {ctx} />
			{:else}
				<!-- Nested objects/arrays now expand inline instead of collapsing to compactJson. -->
				<SchemaValueView value={v} {ctx} depth={1} />
			{/if}
		</dd>
	{/each}
</dl>
