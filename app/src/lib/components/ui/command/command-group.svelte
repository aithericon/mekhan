<script lang="ts">
	import { Command as CommandPrimitive } from 'bits-ui';
	import type { Snippet } from 'svelte';
	import { cn, type WithoutChildrenOrChild } from '$lib/utils.js';

	let {
		ref = $bindable(null),
		class: className,
		heading,
		children,
		value,
		...restProps
	}: WithoutChildrenOrChild<CommandPrimitive.GroupProps> & {
		heading?: Snippet | string;
		children: Snippet;
		value?: string;
	} = $props();
</script>

<CommandPrimitive.Group
	bind:ref
	data-slot="command-group"
	data-command-group
	class={cn('text-foreground overflow-hidden p-1', className)}
	{value}
	{...restProps}
>
	{#if heading}
		<CommandPrimitive.GroupHeading
			data-command-group-heading
			class="text-muted-foreground px-2 py-1.5 text-xs font-medium"
		>
			{#if typeof heading === 'string'}
				{heading}
			{:else}
				{@render heading?.()}
			{/if}
		</CommandPrimitive.GroupHeading>
	{/if}
	<CommandPrimitive.GroupItems>
		{@render children?.()}
	</CommandPrimitive.GroupItems>
</CommandPrimitive.Group>
