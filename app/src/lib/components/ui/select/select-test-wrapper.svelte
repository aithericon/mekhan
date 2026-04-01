<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import * as Select from './index';

	let {
		value = $bindable(''),
		placeholder = 'Select an option',
		items = [] as Array<{ value: string; label: string }>,
		disabled = false,
		open = $bindable(false)
	}: {
		value?: string;
		placeholder?: string;
		items?: Array<{ value: string; label: string }>;
		disabled?: boolean;
		open?: boolean;
	} = $props();
</script>

<Select.Root type="single" bind:value bind:open {disabled}>
	<Select.Trigger data-testid="select-trigger">
		{#if value}
			<span data-testid="select-value">{items.find((i) => i.value === value)?.label ?? value}</span>
		{:else}
			<span data-testid="select-placeholder">{placeholder}</span>
		{/if}
	</Select.Trigger>
	<Select.Content>
		{#each items as item (item.value)}
			<Select.Item value={item.value} label={item.label} />
		{/each}
	</Select.Content>
</Select.Root>
