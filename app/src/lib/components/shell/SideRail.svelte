<script lang="ts">
	// The canonical left rail for PageShell's `sidebar` snippet — ONE width and
	// chrome for every tree/filter rail (Templates, Folders, …) so sidebar
	// pages line up page-to-page. `min-h-full` keeps the border/bg running the
	// rail's full height inside the shell's scroll wrapper.
	import type { Snippet } from 'svelte';
	import { cn } from '$lib/utils.js';

	let {
		testid,
		class: className,
		children
	}: {
		/** data-testid on the <aside>. */
		testid?: string;
		/** Extra classes (NOT width overrides — the rail width is the point). */
		class?: string;
		children: Snippet;
	} = $props();
</script>

<aside
	class={cn('min-h-full w-72 shrink-0 border-r border-border bg-card/30', className)}
	data-testid={testid}
>
	{@render children()}
</aside>
