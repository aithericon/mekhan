<script lang="ts">
	import { cn } from '$lib/utils.js';
	import type { TreeViewRootProps } from './types';

	let { children, class: className, ...rest }: TreeViewRootProps = $props();
</script>

<div role="tree" class={cn('flex flex-col', className)} {...rest}>
	{@render children?.()}
</div>
