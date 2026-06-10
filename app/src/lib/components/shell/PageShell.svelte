<script lang="ts" module>
	/**
	 * Canonical page widths — one per archetype (see README.md):
	 *  - narrow  → max-w-2xl  forms / profile / simple create pages
	 *  - default → max-w-5xl  standard list pages
	 *  - wide    → max-w-6xl  dense operator surfaces (fleet, data, models, dashboard)
	 *  - full    → no max-w   full-width detail pages (keeps px-6 padding + scroll)
	 *  - bleed   → opt-out    canvas/editor pages: no scroll, no padding, no width cap
	 */
	export type PageWidth = 'narrow' | 'default' | 'wide' | 'full' | 'bleed';
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';
	import { cn } from '$lib/utils.js';

	let {
		width = 'default',
		testid,
		band,
		class: className,
		children
	}: {
		width?: PageWidth;
		/** data-testid on the outermost page element. */
		testid?: string;
		/**
		 * Optional pinned header band (border-b bg-card, stays put while the
		 * content below scrolls). Use ONLY for layouts whose children scroll
		 * independently under a fixed header + tab bar (e.g. /models). Normal
		 * pages put PageHeader in-flow inside the scroll content instead.
		 */
		band?: Snippet;
		/** Extra classes on the inner width-constrained wrapper. */
		class?: string;
		children: Snippet;
	} = $props();

	const MAX_W: Record<Exclude<PageWidth, 'bleed'>, string> = {
		narrow: 'max-w-2xl',
		default: 'max-w-5xl',
		wide: 'max-w-6xl',
		full: ''
	};
</script>

{#if width === 'bleed'}
	<!-- Full-bleed canvas opt-out: the page owns its own layout/scroll
	     (xyflow et al. need a definite-height, unpadded parent). -->
	<div class="h-full" data-testid={testid}>
		{@render children()}
	</div>
{:else if band}
	<div class="flex h-full flex-col" data-testid={testid}>
		<div class="shrink-0 border-b border-border bg-card px-6 pt-5 pb-3">
			<div class={cn('mx-auto w-full', MAX_W[width])}>
				{@render band()}
			</div>
		</div>
		<div class="flex-1 overflow-y-auto">
			<div class={cn('animate-rise mx-auto w-full px-6 py-6', MAX_W[width], className)}>
				{@render children()}
			</div>
		</div>
	</div>
{:else}
	<!-- The page owns its scroll container (the root layout's <main> is
	     overflow-hidden on purpose) — never move scrolling to the body. -->
	<div class="h-full overflow-y-auto" data-testid={testid}>
		<div class={cn('animate-rise mx-auto w-full px-6 py-8', MAX_W[width], className)}>
			{@render children()}
		</div>
	</div>
{/if}
