<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import { RatingGroup as RatingGroupPrimitive } from 'bits-ui';
	import { cn } from '$lib/utils.js';

	let {
		ref = $bindable(null),
		class: className,
		value = $bindable(0),
		...restProps
	}: RatingGroupPrimitive.RootProps = $props();
</script>

<RatingGroupPrimitive.Root
	bind:ref
	bind:value
	data-slot="rating-group"
	class={cn('flex items-center gap-1', className)}
	{...restProps}
/>
