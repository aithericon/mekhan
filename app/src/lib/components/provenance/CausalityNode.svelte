<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { ProvenanceGraphNode } from '$lib/types/provenance';
	import { getNodeColor, getNodeLabel } from '$lib/utils/provenance-graph';

	interface Props {
		data: ProvenanceGraphNode & {
			onSelect?: (node: ProvenanceGraphNode) => void;
		};
	}

	let { data }: Props = $props();

	const color = $derived(getNodeColor(data));
	const label = $derived(getNodeLabel(data));

	const handlerLabel = $derived(
		data.effect_handler
			? data.effect_handler.replace(/_/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase())
			: null
	);

	const isSignal = $derived(
		data.event_type === 'TokenCreated' &&
			data.tokens.some((t) => t.role === 'produced' && t.place_id.startsWith('sig_'))
	);
	const isSeed = $derived(
		data.event_type === 'TokenCreated' && !isSignal
	);
	const isBridge = $derived(data.event_type === 'TokenBridgedOut');

	const consumedPlaces = $derived(
		[...new Set(data.tokens.filter((t) => t.role === 'consumed').map((t) => t.place_name || t.place_id))]
	);
	const producedPlaces = $derived(
		[...new Set(data.tokens.filter((t) => t.role === 'produced').map((t) => t.place_name || t.place_id))]
	);

	function formatTime(ts: string): string {
		return new Intl.DateTimeFormat(undefined, {
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		}).format(new Date(ts));
	}

	function handleClick(e: MouseEvent) {
		e.stopPropagation();
		data.onSelect?.(data);
	}
</script>

<Handle type="target" position={Position.Top} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	class="causality-node rounded-lg border bg-white px-3 py-2 shadow-sm transition-shadow hover:shadow-md dark:bg-zinc-900"
	style="border-left: 3px solid {color}; min-width: 240px;"
	onclick={handleClick}
>
	<div class="flex items-center gap-2">
		{#if isSignal}
			<span class="inline-block h-2.5 w-2.5 rounded-full bg-orange-400"></span>
		{:else if isSeed}
			<span class="inline-block h-2.5 w-2.5 rounded-full bg-emerald-500"></span>
		{:else if isBridge}
			<span class="inline-block h-2.5 w-2.5 rounded-full border-2 border-orange-500"></span>
		{:else}
			<span class="inline-block h-2.5 w-2.5 rounded-full" style="background: {color}"></span>
		{/if}

		<span class="text-sm font-medium text-zinc-900 dark:text-zinc-100 truncate">
			{label}
		</span>
	</div>

	<div class="mt-1 flex items-center gap-1.5 text-sm text-zinc-500 dark:text-zinc-400">
		{#if handlerLabel}
			<span
				class="rounded px-1 py-0.5 text-sm font-medium"
				style="background: {color}20; color: {color};"
			>
				{handlerLabel}
			</span>
		{/if}

		{#if data.place_name}
			<span class="truncate">{data.place_name}</span>
		{/if}

		<span class="ml-auto tabular-nums">{formatTime(data.timestamp)}</span>
	</div>

	{#if consumedPlaces.length > 0 || producedPlaces.length > 0}
		<div class="mt-1 flex items-center gap-1 text-sm text-zinc-500 dark:text-zinc-400">
			{#if consumedPlaces.length > 0}
				<span class="text-red-400">{consumedPlaces.join(', ')}</span>
			{/if}
			{#if consumedPlaces.length > 0 && producedPlaces.length > 0}
				<span class="text-zinc-300">&rarr;</span>
			{/if}
			{#if producedPlaces.length > 0}
				<span class="text-emerald-500">{producedPlaces.join(', ')}</span>
			{/if}
		</div>
	{/if}

	{#if data.net_id}
		<div class="mt-0.5 text-sm text-zinc-400 dark:text-zinc-500 truncate">
			{data.net_id}
		</div>
	{/if}
</div>

<Handle type="source" position={Position.Bottom} />
