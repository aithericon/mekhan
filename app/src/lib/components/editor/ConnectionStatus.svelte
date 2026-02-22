<script lang="ts">
	import { onDestroy } from 'svelte';
	import type { MekhanWsProvider } from '$lib/yjs/ws-provider';
	import Circle from '@lucide/svelte/icons/circle';

	type Props = {
		provider: MekhanWsProvider;
	};

	let { provider }: Props = $props();

	let status = $state<'connected' | 'connecting' | 'disconnected'>('connecting');

	function handleStatus({ status: s }: { status: string }) {
		if (s === 'connected') status = 'connected';
		else if (s === 'connecting') status = 'connecting';
		else status = 'disconnected';
	}

	provider.on('status', handleStatus);

	onDestroy(() => {
		provider.off('status', handleStatus);
	});

	const dotClass = $derived(
		status === 'connected'
			? 'fill-green-500 text-green-500'
			: status === 'connecting'
				? 'fill-amber-500 text-amber-500 animate-pulse'
				: 'fill-red-500 text-red-500'
	);

	const label = $derived(
		status === 'connected'
			? 'Connected'
			: status === 'connecting'
				? 'Reconnecting...'
				: 'Disconnected'
	);
</script>

<div class="flex items-center gap-1 text-[10px] text-muted-foreground">
	<Circle class="size-2 {dotClass}" />
	{label}
</div>
