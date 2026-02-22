<script lang="ts">
	import type { Snippet } from 'svelte';
	import { Select } from 'bits-ui';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import { cn } from '$lib/utils.js';

	type Props = {
		children?: Snippet;
		disabled?: boolean;
		class?: string;
	};

	let { children, disabled, class: className }: Props = $props();
</script>

<Select.Trigger
	{disabled}
	class={cn("flex h-8 w-full items-center justify-between rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground transition-colors focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70 data-[placeholder]:text-muted-foreground", className)}
>
	<span class="truncate">
		{@render children?.()}
	</span>
	<ChevronDown class="ml-1 size-3.5 shrink-0 opacity-50" />
</Select.Trigger>
