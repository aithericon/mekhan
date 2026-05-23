<script lang="ts">
	// Universal "what data this step can read" affordance. Strictly a RefPicker
	// trigger — the rich two-column popover already handles grouped browsing
	// and search, so a parallel flat list would be redundant.
	//
	// Capability-by-prop, not by step type:
	//   • `oninsertref` provided → pick inserts the formatted snippet at the
	//     active code editor's cursor (IDE).
	//   • absent → pick is a no-op; the trigger is purely a preview surface
	//     (canvas, or non-code-authored steps).
	// Language-specific guidance (Python SDK helpers, runtime warnings) lives
	// next to its backend's config panel.
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import RefPicker from './RefPicker.svelte';

	type Props = {
		/** This node's in-scope refs (from `/api/analyze`). */
		scope: ScopeEntry[];
		/** Optional refresh affordance (IDE re-runs the analyzer; canvas
		 *  auto-refetches on graph edits, so usually omits this). */
		busy?: boolean;
		onRefresh?: () => void;
		/** Edges into this step. When >1 and the step isn't a Parallel Join,
		 *  scope is the union across branches — surface a warning. */
		incomingCount?: number;
		/** When provided, picking a ref inserts the formatted snippet at the
		 *  active code editor's cursor. Otherwise picking is a no-op. */
		oninsertref?: (snippet: string) => void;
		/** Maps a qualified ref to the snippet the parent's editor expects.
		 *  Defaults to the qualified form itself — already correct for
		 *  Python's direct slug access and Rhai identifiers. */
		format?: (qualified: string) => string;
	};

	let {
		scope,
		busy = false,
		onRefresh,
		incomingCount = 0,
		oninsertref,
		format = (q) => q
	}: Props = $props();

	const unmergedFanIn = $derived(incomingCount > 1);

	const triggerLabel = $derived.by(() => {
		if (scope.length === 0) return 'No upstream fields in scope';
		if (oninsertref) return 'Insert variable…';
		const n = scope.length;
		return `Browse ${n} field${n === 1 ? '' : 's'} in scope…`;
	});

	function onpick(entry: ScopeEntry) {
		oninsertref?.(format(entry.qualified));
	}
</script>

<div class="space-y-1.5" data-testid="in-scope-refs">
	<div class="flex items-center justify-between gap-2">
		<span class="text-sm font-medium text-muted-foreground">
			Inputs in scope
			{#if unmergedFanIn}
				<span
					class="ml-1.5 inline-flex items-center rounded bg-amber-100 px-1.5 py-px text-sm font-medium text-amber-900"
					title="Unmerged fan-in: {incomingCount} branches feed this step without a Parallel Join, so each run only sees one branch's data. The picker shows the union across branches."
					data-testid="in-scope-refs-fanin-warning"
				>
					⚠ fan-in ({incomingCount})
				</span>
			{/if}
		</span>
		{#if onRefresh}
			<button
				type="button"
				class="shrink-0 rounded px-1.5 py-0.5 text-sm text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-50"
				disabled={busy}
				onclick={() => onRefresh?.()}
				title="Recompute scope from the live graph"
				data-testid="in-scope-refs-refresh"
			>
				{busy ? 'Refreshing…' : 'Refresh'}
			</button>
		{/if}
	</div>

	<RefPicker
		{scope}
		disabled={scope.length === 0}
		placeholder={triggerLabel}
		{onpick}
	/>

	{#if scope.length === 0}
		<p class="text-sm text-muted-foreground">
			Wire a Start or AutomatedStep upstream and declare its output port to
			reference fields here.
		</p>
	{/if}
</div>
