<script lang="ts">
	import { Select } from 'bits-ui';
	import Check from '@lucide/svelte/icons/check';

	type Props = {
		value: string;
		label?: string;
		disabled?: boolean;
		class?: string;
	};

	let { value, label, disabled, class: className }: Props = $props();
</script>

<Select.Item
	{value}
	{label}
	{disabled}
	class="relative flex w-full cursor-default select-none items-center rounded-sm py-1.5 pl-7 pr-2 text-sm outline-none data-[disabled]:pointer-events-none data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground data-[disabled]:opacity-50 {className ?? ''}"
>
	{#snippet children({ selected })}
		<span class="absolute left-2 flex size-3.5 items-center justify-center">
			{#if selected}
				<Check class="size-3.5" />
			{/if}
		</span>
		{label ?? value}
	{/snippet}
</Select.Item>
