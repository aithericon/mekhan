<script lang="ts">
	// Universal "what data this step can read" panel. Renders the qualified
	// refs in scope (clean-cut `<slug>.<field>` or `input.<path>`) grouped by
	// producer — the same scope the compiler resolves via `/api/analyze`.
	//
	// Capability-by-prop, not by step type:
	//   • `oninsertref` provided → each row is a click-to-insert button and a
	//     searchable RefPicker appears on top (IDE + active code editor).
	//   • absent → static read-only chips (canvas, or non-code-authored steps).
	// Strictly the picker. Language-specific guidance (Python SDK helpers,
	// runtime warnings) lives next to its backend's config panel.
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
		 *  the scope below is the union across branches — surface a warning. */
		incomingCount?: number;
		/** When provided, every ref becomes a click-to-insert button. The
		 *  parent wires this to the active code editor's `insertAtCursor`. */
		oninsertref?: (snippet: string) => void;
		/** Maps a qualified ref to the snippet the parent's editor expects.
		 *  Defaults to the qualified form itself, which is Python's direct
		 *  slug access (`review.invoice_amount`) and Rhai's identifier form. */
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

	type Group = { key: string; label: string; isProcess: boolean; entries: ScopeEntry[] };
	const groups = $derived.by(() => {
		const out: Group[] = [];
		for (const e of scope) {
			const key = `${e.nodeId} ${e.nodeLabel}`;
			let g = out.find((x) => x.key === key);
			if (!g) {
				g = { key, label: e.nodeLabel, isProcess: e.nodeLabel === 'Process', entries: [] };
				out.push(g);
			}
			g.entries.push(e);
		}
		return out.sort((a, b) => Number(a.isProcess) - Number(b.isProcess));
	});

	function insert(entry: ScopeEntry) {
		oninsertref?.(format(entry.qualified));
	}
</script>

<details class="group rounded-md border border-border/60 bg-muted/10" open>
	<summary
		class="flex list-none cursor-pointer select-none items-center justify-between gap-2 px-2.5 py-1.5 text-sm font-medium text-muted-foreground hover:text-foreground [&::-webkit-details-marker]:hidden"
	>
		<span class="flex items-baseline gap-2">
			<span>Inputs in scope</span>
			<span class="font-normal text-muted-foreground/80">
				<code class="font-mono">&lt;slug&gt;.&lt;field&gt;</code> ·
				<code class="font-mono">input.&lt;path&gt;</code>
			</span>
		</span>
		<span class="flex items-center gap-2">
			{#if onRefresh}
				<button
					type="button"
					class="rounded px-1.5 py-0.5 text-sm text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-50"
					disabled={busy}
					onclick={(e) => {
						e.preventDefault();
						e.stopPropagation();
						onRefresh?.();
					}}
					title="Recompute scope from the live graph"
					data-testid="in-scope-refs-refresh"
				>
					{busy ? 'Refreshing…' : 'Refresh'}
				</button>
			{/if}
			<span class="text-muted-foreground transition-transform group-open:rotate-90">›</span>
		</span>
	</summary>

	<div class="space-y-3 px-2.5 pb-2.5 pt-1" data-testid="in-scope-refs">
		{#if unmergedFanIn}
			<div
				class="rounded-md border border-amber-300 bg-amber-50 px-2.5 py-2 text-sm leading-snug text-amber-900"
			>
				<span class="font-semibold">⚠ Unmerged fan-in ({incomingCount} inputs).</span>
				This step isn't a Parallel Join, so it <strong>runs once per upstream
				token</strong> — each run sees only that branch's data. The fields below
				are the <em>union across all branches</em>, not what's present in a
				single run. Insert a <strong>Parallel Join</strong> upstream to combine
				inputs into one token.
			</div>
		{/if}

		{#if scope.length > 0 && oninsertref}
			<!-- Compact searchable picker — same component the Decision branch
			     editor uses. Picking inserts the formatted snippet at cursor. -->
			<RefPicker
				{scope}
				placeholder="Insert variable…"
				onpick={(e) => insert(e)}
			/>
		{/if}

		{#if groups.length > 0}
			<ul class="space-y-2">
				{#each groups as g (g.key)}
					<li class="space-y-0.5">
						<div
							class="text-sm uppercase tracking-wider {g.isProcess
								? 'italic text-muted-foreground'
								: 'text-muted-foreground'}"
						>
							{g.label}
						</div>
						<ul class="space-y-0.5">
							{#each g.entries as e (e.qualified)}
								<li>
									{#if oninsertref}
										<button
											type="button"
											class="flex w-full items-baseline justify-between gap-2 rounded px-1.5 py-0.5 text-left text-sm transition-colors hover:bg-accent hover:text-foreground"
											onclick={() => insert(e)}
											title={`Insert ${format(e.qualified)} at cursor`}
											data-testid="in-scope-refs-entry"
										>
											<code class="font-mono text-foreground">{e.qualified}</code>
											<span class="shrink-0 text-sm text-muted-foreground">{e.kind}</span>
										</button>
									{:else}
										<div
											class="flex items-baseline justify-between gap-2 px-1.5 py-0.5 text-sm"
											data-testid="in-scope-refs-entry"
										>
											<code class="font-mono text-foreground">{e.qualified}</code>
											<span class="shrink-0 text-sm text-muted-foreground">{e.kind}</span>
										</div>
									{/if}
								</li>
							{/each}
						</ul>
					</li>
				{/each}
			</ul>
		{:else}
			<p class="text-sm text-muted-foreground">
				No upstream fields reach this step yet. Wire a Start or AutomatedStep
				upstream and declare its output port to reference fields here.
			</p>
		{/if}
	</div>
</details>
