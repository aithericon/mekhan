<script lang="ts">
	// The Fleet page — two lenses over the same hardware/people:
	//   "Pools"    (default) — the logical dispatch units. One row per capacity,
	//                grouped by operator-vocabulary kind (pool-kinds.ts): machine /
	//                worker / human pools, limits, clusters, not-dispatchable.
	//   "Machines" — the flat physical inventory of every enrolled daemon (runners
	//                AND workers), identity-first; pools appear only as linked
	//                chips per row (MachinesTable owns its own fetching + toolbar).
	//
	// Deep-link compat: the old Control-Plane params still land correctly —
	// `?tab=capacities` → Pools, `?tab=runners` → Machines, and `?role=engines`
	// (the Engines lens from /models) forces the Machines tab filtered to model
	// servers. Tab switches reflect into the URL via history.replaceState.
	import { page } from '$app/state';
	import * as Tabs from '$lib/components/ui/tabs';
	import { PageShell, PageHeader } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import Plus from '@lucide/svelte/icons/plus';
	import { listCapacities, type CapacitySummary } from '$lib/api/capacities';
	import { deleteResource } from '$lib/api/resources';
	import { reconnectCluster, drainCluster } from '$lib/api/clusters';
	import PoolsList from '$lib/components/fleet/PoolsList.svelte';
	import MachinesTable from '$lib/components/fleet/MachinesTable.svelte';
	import NewCapacityModal from '$lib/components/fleet/NewCapacityModal.svelte';
	import EnrollSheet from '$lib/components/fleet/EnrollSheet.svelte';

	// ── Tabs + deep-link compat ─────────────────────────────────────────────────

	type Tab = 'pools' | 'machines';

	function initialTab(): Tab {
		const sp = page.url.searchParams;
		if (sp.get('role') === 'engines') return 'machines';
		const t = sp.get('tab');
		if (t === 'machines' || t === 'runners') return 'machines';
		return 'pools'; // 'pools', legacy 'capacities', absent, or unknown
	}

	let activeTab = $state<Tab>(initialTab());
	const roleParam: 'all' | 'engines' =
		page.url.searchParams.get('role') === 'engines' ? 'engines' : 'all';

	function onTab(v: string) {
		activeTab = v === 'machines' ? 'machines' : 'pools';
		if (typeof window !== 'undefined') {
			const url = new URL(window.location.href);
			url.searchParams.set('tab', activeTab);
			history.replaceState(null, '', url);
		}
	}

	// ── State ──────────────────────────────────────────────────────────────────

	let capacities = $state<CapacitySummary[]>([]);
	let error = $state<string | null>(null);
	let lastUpdated = $state<Date | null>(null);

	// "New pool" → the kind-switcher modal. `editing` non-null ⇒ the same modal
	// in edit mode (kind + name locked, fields prefilled).
	let createOpen = $state(false);
	let editing = $state<CapacitySummary | null>(null);
	// A machine-pool row's "Enroll" → EnrollSheet scoped to that pool's path.
	let enrollOpen = $state(false);
	let enrollGroup = $state<string | null>(null);
	// A worker-pool row's "Enroll" → EnrollSheet in worker mode, scoped likewise.
	let enrollWorkerOpen = $state(false);
	let enrollWorkerGroup = $state<string | null>(null);

	// ── Polling ────────────────────────────────────────────────────────────────

	async function poll() {
		try {
			capacities = await listCapacities();
			lastUpdated = new Date();
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to fetch pools';
		}
	}

	$effect(() => {
		void poll();
		const t = setInterval(() => {
			void poll();
		}, 5000);
		return () => clearInterval(t);
	});

	const updatedLabel = $derived(
		lastUpdated ? lastUpdated.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : null
	);

	// ── Pool actions ───────────────────────────────────────────────────────────

	function openCreate() {
		editing = null;
		createOpen = true;
	}

	function onSaved() {
		createOpen = false;
		editing = null;
		void poll();
	}

	/** A machine-pool row's "Enroll" — scope the runner sheet to that pool's path. */
	function onEnrollMachine(path: string) {
		enrollGroup = path;
		enrollOpen = true;
	}

	/** A worker-pool row's "Enroll" — scope the worker sheet to that pool's path. */
	function onEnrollWorker(path: string) {
		enrollWorkerGroup = path;
		enrollWorkerOpen = true;
	}

	// Edit opens the same modal in edit mode, prefilled from the summary the page
	// already holds (kind + name locked; count / cluster fields editable).
	function onEditPool(id: string) {
		editing = capacities.find((c) => c.id === id) ?? null;
		createOpen = true;
	}

	async function onDeletePool(id: string) {
		const cap = capacities.find((c) => c.id === id);
		const label = cap ? cap.display_name || cap.path : 'this pool';
		if (!confirm(`Delete pool “${label}”? Its backing net (if any) is retired.`)) return;
		try {
			await deleteResource(id);
			await poll();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to delete pool';
		}
	}

	/** Cluster row → force the cluster session to reconnect. */
	async function onReconnect(id: string) {
		try {
			await reconnectCluster(id);
			await poll();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Reconnect failed';
		}
	}

	/** Cluster row → drain: refuse new leases while held ones finish. */
	async function onDrain(id: string) {
		try {
			await drainCluster(id);
			await poll();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Drain failed';
		}
	}
</script>

<PageShell width="wide" testid="fleet-page">
	{#snippet band()}
		<PageHeader
			title="Fleet"
			subtitle="Pools of machines, workers and people that pick up work — and the limits and clusters that bound it."
		>
			{#snippet actions()}
				{#if activeTab === 'pools'}
					<Button
						variant="default"
						size="sm"
						class="gap-1.5"
						onclick={openCreate}
						data-testid="new-pool-button"
					>
						<Plus class="size-4" />
						New pool
					</Button>
				{/if}
			{/snippet}
		</PageHeader>
	{/snippet}
	{#snippet tabs()}
		<Tabs.Root value={activeTab} onValueChange={onTab}>
			<Tabs.List variant="underline">
				<Tabs.Trigger variant="underline" value="pools" data-testid="fleet-tab-pools">
					Pools
				</Tabs.Trigger>
				<Tabs.Trigger variant="underline" value="machines" data-testid="fleet-tab-machines">
					Machines
				</Tabs.Trigger>
			</Tabs.List>
		</Tabs.Root>
	{/snippet}

	{#if activeTab === 'pools'}
		{#if error}
			<div
				class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200"
			>
				{error}
			</div>
		{/if}

		{#if updatedLabel}
			<p class="mb-4 text-xs text-muted-foreground">Updated {updatedLabel}</p>
		{/if}

		<PoolsList
			{capacities}
			onenroll={onEnrollMachine}
			onenrollworker={onEnrollWorker}
			onedit={onEditPool}
			ondelete={onDeletePool}
			onreconnect={onReconnect}
			ondrain={onDrain}
		/>
	{:else}
		<!-- The flat physical inventory: every enrolled daemon, runners AND
			 workers. MachinesTable owns its own polling + toolbar (filters,
			 enroll/tokens) when unscoped. -->
		<MachinesTable role={roleParam} />
	{/if}
</PageShell>

<!-- Pool create/edit flow: the dedicated kind-switcher modal. `editing` null ⇒
	 create; a summary ⇒ edit (kind + name locked, fields prefilled). -->
<NewCapacityModal bind:open={createOpen} {editing} onsaved={onSaved} />

<!-- Machine-pool enroll flow, scoped to the row's pool path. -->
<EnrollSheet bind:open={enrollOpen} group={enrollGroup} onenrolled={() => void poll()} />

<!-- Worker-pool enroll flow, scoped to the row's pool path. -->
<EnrollSheet
	bind:open={enrollWorkerOpen}
	mode="worker"
	group={enrollWorkerGroup}
	onenrolled={() => void poll()}
/>
