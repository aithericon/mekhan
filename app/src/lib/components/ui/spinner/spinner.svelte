<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import { cn } from '$lib/utils.js';

	let {
		size = 'default',
		class: className
	}: {
		size?: 'sm' | 'default' | 'lg';
		class?: string;
	} = $props();

	const sizes = { sm: 'size-4', default: 'size-5', lg: 'size-6' } as const;
</script>

<svg
	class={cn('animate-spin', sizes[size], className)}
	viewBox="0 0 24 24"
	fill="none"
	aria-hidden="true"
>
	<circle cx="12" cy="12" r="10" stroke="currentColor" stroke-width="3" class="opacity-25" />
	<path
		d="M12 2a10 10 0 0 1 10 10"
		stroke="currentColor"
		stroke-width="3"
		stroke-linecap="round"
		class="opacity-75"
	/>
</svg>
