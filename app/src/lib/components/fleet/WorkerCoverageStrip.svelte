<script lang="ts">
	// A SLIM fleet-wide worker backend-coverage summary. Workers are a GLOBAL
	// fleet (anonymous competing-consumer executor daemons, NOT enrolled runners),
	// so their coverage is a single horizontal strip — one chip per ExecutorJob
	// backend with its live worker_count. Uncovered (count 0) backends are flagged
	// (amber/destructive tone + a TriangleAlert) since steps on them queue at
	// `submitted` until a worker connects. This is the condensed counterpart to the
	// full WorkerPoolBoard — same data, one compact row.
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import {
		getWorkerCoverage,
		type BackendCoverageEntry
	} from '$lib/api/workers';

	let backends = $state<BackendCoverageEntry[]>([]);
	let error = $state<string | null>(null);

	const coveredCount = $derived(backends.filter((b) => b.worker_count > 0).length);
	const uncovered = $derived(backends.filter((b) => b.worker_count === 0));

	async function poll() {
		try {
			const snap = await getWorkerCoverage();
			backends = snap.backends;
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to fetch worker coverage';
		}
	}

	$effect(() => {
		void poll();
		const t = setInterval(() => {
			void poll();
		}, 5000);
		return () => clearInterval(t);
	});
</script>

<div
	class="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-lg border border-border bg-card px-3 py-2"
	data-testid="worker-coverage-strip"
>
	<span class="text-xs font-medium text-muted-foreground">
		Backend coverage · <span class="tabular-nums text-foreground">{coveredCount}</span>/<span
			class="tabular-nums">{backends.length}</span
		>
	</span>

	{#if error}
		<span class="text-xs text-amber-700 dark:text-amber-300">{error}</span>
	{:else}
		<div class="flex flex-wrap items-center gap-1.5">
			{#each backends as b (b.backend)}
				{@const covered = b.worker_count > 0}
				<span
					class="inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs
						{covered
						? 'border-border bg-background text-foreground'
						: 'border-amber-200 bg-amber-50 text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200'}"
					data-testid="coverage-chip-{b.backend}"
				>
					{#if !covered}
						<TriangleAlert class="size-3 shrink-0" />
					{/if}
					<span class="font-mono">{b.backend}</span>
					<span class="tabular-nums opacity-70">{b.worker_count}</span>
				</span>
			{/each}
		</div>

		{#if uncovered.length > 0}
			<span class="flex items-center gap-1 text-xs text-amber-700 dark:text-amber-300">
				<TriangleAlert class="size-3.5 shrink-0" />
				{uncovered.length} uncovered
			</span>
		{/if}
	{/if}
</div>
