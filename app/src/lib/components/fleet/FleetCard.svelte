<script lang="ts">
	// The shared representation of one fleet unit — a runner station (Live board)
	// OR an anonymous worker (Workers tab). Both are "a node with a liveness dot,
	// a name, a meta line, and a set of advertised backends, with a hover tooltip":
	// PresenceBoard and WorkerPoolBoard each had their own near-identical card, so
	// they're unified here.
	import type { Snippet } from 'svelte';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import StatusDot from './StatusDot.svelte';
	import BackendChips from './BackendChips.svelte';

	let {
		title,
		tone = 'idle',
		meta,
		backends = [],
		tooltip,
		testid
	}: {
		title: string;
		/** Liveness tone — 'live' tints the dot + card border emerald. */
		tone?: 'live' | 'idle' | 'warn';
		/** One-line status/meta (e.g. "Online · 3s ago", "2 backends · 5s ago"). */
		meta?: string;
		/** Advertised backends; pass `[]` (or omit) to render no chips. Callers gate
		    visibility themselves (e.g. a runner advertises only while online). */
		backends?: string[];
		/** Optional hover-tooltip body. */
		tooltip?: Snippet;
		testid?: string;
	} = $props();
</script>

<Tooltip.Provider>
	<Tooltip.Root>
		<Tooltip.Trigger class="w-full text-left">
			<div
				class="flex flex-col gap-2 rounded-lg border bg-card p-4 transition-colors hover:bg-accent/40
					{tone === 'live' ? 'border-emerald-200 dark:border-emerald-800/50' : 'border-border'}"
				data-testid={testid}
			>
				<div class="flex min-w-0 items-center gap-2">
					<StatusDot {tone} />
					<span class="truncate text-sm font-medium text-foreground">{title}</span>
				</div>
				{#if meta}
					<p class="text-sm text-muted-foreground">{meta}</p>
				{/if}
				{#if backends.length > 0}
					<BackendChips {backends} />
				{/if}
			</div>
		</Tooltip.Trigger>
		{#if tooltip}
			<Tooltip.Content side="top">{@render tooltip()}</Tooltip.Content>
		{/if}
	</Tooltip.Root>
</Tooltip.Provider>
