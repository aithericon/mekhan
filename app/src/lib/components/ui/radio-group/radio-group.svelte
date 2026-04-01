<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import { RadioGroup as RadioGroupPrimitive } from 'bits-ui';
	import { cn } from '$lib/utils.js';

	let {
		ref = $bindable(null),
		class: className,
		value = $bindable(''),
		...restProps
	}: RadioGroupPrimitive.RootProps = $props();
</script>

<RadioGroupPrimitive.Root
	bind:ref
	bind:value
	data-slot="radio-group"
	class={cn('grid gap-3', className)}
	{...restProps}
/>
