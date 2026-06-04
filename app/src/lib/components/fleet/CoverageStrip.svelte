<script lang="ts">
	// A compact PER-GROUP backend-coverage strip: one chip per backend this group
	// covers, carrying the COUNT of present members advertising it (e.g.
	// `python 3 · docker 1`). This is the per-group home for backend coverage —
	// a fleet-wide global blob was noise (every backend, mostly zero), but scoped
	// to one group "how many of my live members can run python?" is the real
	// question. Replaces the old number-less `covers` chips on the presence board.
	type Entry = { backend: string; count: number };
	let {
		entries,
		label = 'covers'
	}: {
		/** Backends this group covers, each with its present-member count. */
		entries: Entry[];
		/** Leading label (e.g. "covers"); omit with `label=""`. */
		label?: string;
	} = $props();
</script>

{#if entries.length > 0}
	<div class="flex flex-wrap items-center gap-1.5" data-testid="coverage-strip">
		{#if label}<span class="text-sm text-muted-foreground">{label}</span>{/if}
		{#each entries as e (e.backend)}
			<span
				class="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2 py-0.5 text-xs"
				data-testid="coverage-chip-{e.backend}"
			>
				<span class="font-mono text-foreground">{e.backend}</span>
				<span class="tabular-nums text-muted-foreground">{e.count}</span>
			</span>
		{/each}
	</div>
{/if}
