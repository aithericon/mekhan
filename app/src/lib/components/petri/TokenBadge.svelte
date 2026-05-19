<script lang="ts">
	import type { Token } from '$lib/types/petri';
	import * as Tooltip from '$lib/components/ui/tooltip';

	interface Props {
		token: Token;
		size?: 'sm' | 'md';
	}

	let { token, size = 'md' }: Props = $props();

	const colorType = $derived(token.color.type);
	const colorValue = $derived((token.color as any).value);

	const badgeClass = $derived.by(() => {
		switch (colorType) {
			case 'Unit':
				return 'bg-gray-300 dark:bg-gray-800';
			case 'Integer':
				return 'bg-violet-600';
			case 'Data':
				return 'bg-pink-500';
			default:
				return 'bg-gray-500';
		}
	});

	const sizeClass = $derived(size === 'sm' ? 'w-3 h-3' : 'w-4 h-4');
	const textClass = $derived(size === 'sm' ? 'text-sm' : 'text-sm');

	const displayValue = $derived.by(() => {
		if (colorType === 'Unit') return '';
		if (colorType === 'Integer') return colorValue?.toString() ?? '';
		if (colorType === 'Data') {
			// Try to show a meaningful short value
			if (colorValue?.worker_id) return colorValue.worker_id;
			if (colorValue?.task_id) return colorValue.task_id;
			return 'D';
		}
		return '';
	});

	const tooltipContent = $derived.by(() => {
		if (colorType === 'Unit') return 'Unit token';
		if (colorType === 'Integer') return `Integer: ${colorValue}`;
		if (colorType === 'Data') return JSON.stringify(colorValue, null, 2);
		return colorType;
	});
</script>

<Tooltip.Root>
	<Tooltip.Trigger>
		<div
			class="token-badge rounded-full {badgeClass} {sizeClass} flex items-center justify-center"
		>
			{#if displayValue}
				<span class="{textClass} text-white font-bold">{String(displayValue).slice(0, 2)}</span>
			{/if}
		</div>
	</Tooltip.Trigger>
	<Tooltip.Content side="top" class="max-w-64 bg-popover text-popover-foreground shadow-lg border border-border">
		<pre class="text-sm font-mono whitespace-pre-wrap">{tooltipContent}</pre>
	</Tooltip.Content>
</Tooltip.Root>

<style>
	.token-badge {
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.3);
	}
</style>
