<script lang="ts" module>
	/**
	 * Canonical page widths — one per archetype (see README.md):
	 *  - narrow  → max-w-2xl  form BODIES (profile, simple create pages)
	 *  - default → max-w-6xl  the standard page width (lists, operator surfaces)
	 *  - wide    → max-w-6xl  alias of default (kept for call-site readability)
	 *  - full    → no max-w   full-width detail bodies (keeps px-6 padding + scroll)
	 *  - bleed   → opt-out    canvas/editor pages: no scroll, no padding, no width cap
	 *
	 * The width variant caps the BODY only. The band's inner content always
	 * aligns to the max-w-6xl grid (BAND_MAX_W) so the title/tabs sit at the
	 * same x on every page — page-to-page navigation must never make the
	 * header jump horizontally.
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
		tabs,
		class: className,
		children
	}: {
		width?: PageWidth;
		/** data-testid on the outermost page element. */
		testid?: string;
		/**
		 * The pinned header band — THE default page anatomy: a full-width
		 * bg-card strip with border-b, pinned while the body scrolls beneath
		 * it. Put the PageHeader (title / subtitle / actions) in here. The
		 * band neutralizes PageHeader's in-flow bottom margin, so no
		 * `class="mb-*"` juggling is needed.
		 *
		 * Pages without `band` fall back to the legacy in-flow layout
		 * (PageHeader inside the scroll content) — acceptable during
		 * migration, but new pages should use the band.
		 */
		band?: Snippet;
		/**
		 * Optional page-level tab row rendered FLUSH on the band's bottom
		 * edge (GitHub-style: the active 2px underline overlaps the band's
		 * border-b via -mb-px). Put a <PageTabs> (link tabs) or a
		 * <Tabs.List variant="underline"> trigger row (state tabs) in here —
		 * never tab CONTENT, which belongs in `children`.
		 */
		tabs?: Snippet;
		/** Extra classes on the inner width-constrained wrapper. */
		class?: string;
		children: Snippet;
	} = $props();

	const MAX_W: Record<Exclude<PageWidth, 'bleed'>, string> = {
		narrow: 'max-w-2xl',
		default: 'max-w-6xl',
		wide: 'max-w-6xl',
		full: ''
	};

	// One grid for every header: band content (title row + tabs) always
	// aligns to this, independent of the body's width variant.
	const BAND_MAX_W = 'max-w-6xl';
</script>

{#if width === 'bleed'}
	<!-- Full-bleed canvas opt-out: the page owns its own layout/scroll
	     (xyflow et al. need a definite-height, unpadded parent). -->
	<div class="h-full" data-testid={testid}>
		{@render children()}
	</div>
{:else if band || tabs}
	<!-- The canonical band anatomy: pinned bg-card header strip (title row +
	     optional flush tab row) over an independently-scrolling body. The
	     `[&>header]:mb-0` zeroes PageHeader's in-flow margin inside the band;
	     the tab row's `-mb-px` pulls the 2px active underline down so it
	     overlaps the band's border-b exactly (GitHub-style). -->
	<div class="flex h-full flex-col" data-testid={testid}>
		<div class={cn('shrink-0 border-b border-border bg-card px-6 pt-5', !tabs && 'pb-4')}>
			<div class={cn('mx-auto w-full', BAND_MAX_W)}>
				{#if band}
					<div class="[&>header]:mb-0">
						{@render band()}
					</div>
				{/if}
				{#if tabs}
					<div class={cn('-mb-px', band && 'mt-2')}>
						{@render tabs()}
					</div>
				{/if}
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
