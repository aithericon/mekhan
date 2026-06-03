<script lang="ts">
	// Fleet management page — thin route wrapper.
	// Three in-page tabs: "Runners" (enrolled lab PCs: list + enroll), "Live
	// board" (runner presence grid), and "Worker pool" (anonymous
	// competing-consumer executor workers + per-backend coverage).
	import RunnerList from '$lib/components/fleet/RunnerList.svelte';
	import PresenceBoard from '$lib/components/fleet/PresenceBoard.svelte';
	import WorkerPoolBoard from '$lib/components/fleet/WorkerPoolBoard.svelte';

	type Tab = 'runners' | 'board' | 'workers';
	let activeTab = $state<Tab>('runners');
</script>

<svelte:head><title>Fleet | Mekhan</title></svelte:head>

<div class="h-full overflow-y-auto" data-testid="fleet-page">
	<div class="mx-auto max-w-5xl px-6 py-8 animate-rise">
		<div class="mb-6">
			<h1 class="text-2xl font-semibold tracking-tight text-foreground">Fleet</h1>
			<p class="mt-1 text-sm text-muted-foreground">
				Lab runners — executor daemons that pick up jobs from your presence pools. Enroll new
				runners with a one-time registration token and monitor them in real time.
			</p>
		</div>

		<!-- Tab bar -->
		<div class="mb-6 flex gap-1 rounded-lg border border-border bg-muted/40 p-1 w-fit">
			<button
				type="button"
				onclick={() => (activeTab = 'runners')}
				class="rounded-md px-4 py-1.5 text-sm font-medium transition-colors
					{activeTab === 'runners'
						? 'bg-background text-foreground shadow-sm'
						: 'text-muted-foreground hover:text-foreground'}"
				data-testid="tab-runners"
			>
				Runners
			</button>
			<button
				type="button"
				onclick={() => (activeTab = 'board')}
				class="rounded-md px-4 py-1.5 text-sm font-medium transition-colors
					{activeTab === 'board'
						? 'bg-background text-foreground shadow-sm'
						: 'text-muted-foreground hover:text-foreground'}"
				data-testid="tab-board"
			>
				Live board
			</button>
			<button
				type="button"
				onclick={() => (activeTab = 'workers')}
				class="rounded-md px-4 py-1.5 text-sm font-medium transition-colors
					{activeTab === 'workers'
						? 'bg-background text-foreground shadow-sm'
						: 'text-muted-foreground hover:text-foreground'}"
				data-testid="tab-workers"
			>
				Worker pool
			</button>
		</div>

		{#if activeTab === 'runners'}
			<RunnerList />
		{:else if activeTab === 'board'}
			<PresenceBoard />
		{:else}
			<WorkerPoolBoard />
		{/if}
	</div>
</div>
