<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import { RatingGroup as RatingGroupPrimitive } from 'bits-ui';
	import Star from '@lucide/svelte/icons/star';
	import { cn } from '$lib/utils.js';

	let {
		ref = $bindable(null),
		class: className,
		...restProps
	}: RatingGroupPrimitive.ItemProps = $props();
</script>

<RatingGroupPrimitive.Item
	bind:ref
	data-slot="rating-group-item"
	class={cn(
		'cursor-pointer rounded p-0.5 transition-transform hover:scale-110 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none',
		className
	)}
	{...restProps}
>
	{#snippet children({ state })}
		<Star
			class={cn(
				'size-6 transition-colors',
				state === 'active'
					? 'fill-amber-400 text-amber-400'
					: 'text-muted-foreground/30 hover:text-amber-300'
			)}
		/>
	{/snippet}
</RatingGroupPrimitive.Item>
