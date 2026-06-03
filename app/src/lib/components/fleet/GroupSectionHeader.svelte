<script lang="ts">
	// The header for one group section, shared by the Runners list and the Live
	// board (they render the same backed/unbacked/ungrouped split via groupFleet,
	// and previously each inlined this markup). The optional `action` snippet is
	// rendered on the right so a caller can inject view-specific affordances (e.g.
	// the Runners list's "Create group" for an unbacked alias) without forking the
	// header.
	import type { Snippet } from 'svelte';
	import { Badge } from '$lib/components/ui/badge';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import RadioTower from '@lucide/svelte/icons/radio-tower';
	import ArrowUpRight from '@lucide/svelte/icons/arrow-up-right';
	import BackendChips from './BackendChips.svelte';
	import type { FleetSection } from './grouping';

	let { section, action }: { section: FleetSection; action?: Snippet } = $props();
</script>

{#if section.kind === 'unbacked'}
	<div
		class="mb-2 flex flex-wrap items-center gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2"
	>
		<TriangleAlert class="size-4 shrink-0 text-amber-600" />
		<span class="text-sm font-medium text-amber-800 dark:text-amber-400">{section.alias}</span>
		<Badge variant="outline" class="border-amber-500/50 text-sm text-amber-700 dark:text-amber-400"
			>no pool · unbacked</Badge
		>
		<span class="text-sm text-amber-700/90 dark:text-amber-400/80">
			These runners heartbeat but are admitted to no pool — no
			<code class="font-mono">runner_group</code> resource backs this alias.
		</span>
		{#if action}<div class="ml-auto">{@render action()}</div>{/if}
	</div>
{:else if section.kind === 'ungrouped'}
	<div class="mb-2 flex items-center gap-2 border-b border-border pb-1.5">
		<span class="text-sm font-semibold text-muted-foreground">Ungrouped</span>
		<span class="text-sm text-muted-foreground">· not assigned to a group</span>
		{#if action}<div class="ml-auto">{@render action()}</div>{/if}
	</div>
{:else}
	<div class="mb-2 flex flex-wrap items-center gap-2 border-b border-border pb-1.5">
		<RadioTower class="size-4 shrink-0 text-muted-foreground" />
		<span class="text-sm font-semibold text-foreground">{section.alias}</span>
		<Badge variant="outline" class="text-sm">pool ready</Badge>
		<span class="text-sm text-muted-foreground tabular-nums">
			{section.onlineCount}/{section.runners.length} online
		</span>
		{#if section.backends.length > 0}
			<span class="ml-1 text-sm text-muted-foreground">covers</span>
			<BackendChips backends={section.backends} />
		{/if}
		<!-- A backed group's capacity lives in its presence-pool net `pool-<id>`;
		     deep-link straight to its live state (NetWorkbench + PoolContentionView). -->
		<div class="ml-auto flex items-center gap-2">
			{#if section.resource}
				<a
					href="/nets/pool-{section.resource.id}"
					class="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground hover:underline"
					data-testid="view-pool-net-{section.alias}"
				>
					View pool net
					<ArrowUpRight class="size-3.5" />
				</a>
			{/if}
			{#if action}{@render action()}{/if}
		</div>
	</div>
{/if}
