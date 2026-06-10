<script lang="ts">
	// The unified Control Plane — RUNNER-FIRST. The default lens is the Runners
	// roster (the actual enrolled machines, identity-rich: name · host · role ·
	// resident model engines), because that's what an operator means by "what do I
	// have." The four dispatch BACKENDS — PRESENCE (runner groups), QUEUE (worker
	// pools), TOKENS (concurrency limits), SCHEDULER (clusters) — are the *backing*
	// for that capacity and live one tab over under "Capacities" (mirroring
	// `CapacityAxes::backend()` 1:1, read off `GET /api/v1/capacities`, polled ~5s).
	//
	// `?tab=capacities` / `?role=engines` deep-link in — the Engines lens
	// (/models/engines) links here as the roster filtered to model servers.
	//
	// Page-level capacity actions (Capacities tab only):
	//   "New capacity"  → NewCapacityModal (kind switcher: runner group / limit /
	//                      worker / cluster).
	//   card "Enroll here" → EnrollSheet, fixed to that group's path.
	import { page } from '$app/state';
	import * as Tabs from '$lib/components/ui/tabs';
	import { PageShell, PageHeader } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import Plus from '@lucide/svelte/icons/plus';
	import Server from '@lucide/svelte/icons/server';
	import Cpu from '@lucide/svelte/icons/cpu';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import Boxes from '@lucide/svelte/icons/boxes';
	import UserPlus from '@lucide/svelte/icons/user-plus';
	import { listCapacities, type CapacitySummary } from '$lib/api/capacities';
	import { deleteResource } from '$lib/api/resources';
	import { reconnectCluster, drainCluster } from '$lib/api/clusters';
	import RunnerList from '$lib/components/fleet/RunnerList.svelte';
	import CapacitySection from '$lib/components/fleet/CapacitySection.svelte';
	import HumanCapacitySection from '$lib/components/fleet/HumanCapacitySection.svelte';
	import BoardHeader from '$lib/components/fleet/BoardHeader.svelte';
	import NewCapacityModal from '$lib/components/fleet/NewCapacityModal.svelte';
	import EnrollSheet from '$lib/components/fleet/EnrollSheet.svelte';

	// ── State ──────────────────────────────────────────────────────────────────

	let capacities = $state<CapacitySummary[]>([]);
	let error = $state<string | null>(null);
	let lastUpdated = $state<Date | null>(null);

	// Runner-first: the roster is the default lens; the capacity board is one tab
	// over. Deep-linked via `?tab=` / `?role=` (the Engines lens lands here).
	let activeTab = $state<'runners' | 'capacities'>(
		page.url.searchParams.get('tab') === 'capacities' ? 'capacities' : 'runners'
	);
	const roleParam: 'all' | 'engines' =
		page.url.searchParams.get('role') === 'engines' ? 'engines' : 'all';

	// "New capacity" → the dedicated kind-switcher modal. `editing` non-null ⇒ the
	// same modal in edit mode (kind + name locked, fields prefilled).
	let createOpen = $state(false);
	let editing = $state<CapacitySummary | null>(null);
	// "Enroll runner" → EnrollSheet. `enrollGroup` null ⇒ global (picker); a path
	// ⇒ scoped to that presence group (per-card "Enroll here").
	let enrollOpen = $state(false);
	let enrollGroup = $state<string | null>(null);
	// "Enroll worker" → EnrollSheet in worker mode. `enrollWorkerGroup` null ⇒
	// global (picker over `workers` capacities); a path ⇒ scoped to that queue
	// group (per-card "Enroll here").
	let enrollWorkerOpen = $state(false);
	let enrollWorkerGroup = $state<string | null>(null);

	// ── Derived: partition by backend ───────────────────────────────────────────

	// Human capacities are ALSO presence-backed — the `offer` dispatch axis is what
	// distinguishes a self-claiming human pool from a runner group. Split them so
	// the Presence section stays runner-only and humans get their own roster view.
	const humans = $derived(
		capacities.filter((c) => c.backend === 'presence' && c.axes?.dispatch === 'offer')
	);
	const presence = $derived(
		capacities.filter((c) => c.backend === 'presence' && c.axes?.dispatch !== 'offer')
	);
	const queue = $derived(capacities.filter((c) => c.backend === 'queue'));
	// `deferred` shares the Tokens lane (the consume-quota path is a Tokens flavour).
	const tokens = $derived(
		capacities.filter((c) => c.backend === 'tokens' || c.backend === 'deferred')
	);
	const scheduler = $derived(capacities.filter((c) => c.backend === 'scheduler'));

	const summary = $derived(`${capacities.length} capacities across 4 backends`);

	// ── Polling ────────────────────────────────────────────────────────────────

	async function poll() {
		try {
			capacities = await listCapacities();
			lastUpdated = new Date();
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to fetch capacities';
		}
	}

	$effect(() => {
		void poll();
		const t = setInterval(() => {
			void poll();
		}, 5000);
		return () => clearInterval(t);
	});

	function openCreate() {
		editing = null;
		createOpen = true;
	}

	function onCreated() {
		createOpen = false;
		editing = null;
		void poll();
	}

	/** Open the global enroll flow (no fixed group — the sheet shows a picker). */
	function openEnroll() {
		enrollGroup = null;
		enrollOpen = true;
	}

	/** A presence card's "Enroll here" — scope the sheet to that group's path. */
	function onEnrollCapacity(path: string) {
		enrollGroup = path;
		enrollOpen = true;
	}

	/** Open the global worker-enroll flow (no fixed group — the sheet shows a picker). */
	function openEnrollWorker() {
		enrollWorkerGroup = null;
		enrollWorkerOpen = true;
	}

	/** A queue card's "Enroll here" — scope the worker sheet to that group's path. */
	function onEnrollWorkerCapacity(path: string) {
		enrollWorkerGroup = path;
		enrollWorkerOpen = true;
	}

	// Edit opens the same modal in edit mode, prefilled from the summary the page
	// already holds (kind + name locked; count / cluster fields editable).
	function onEditCapacity(id: string) {
		editing = capacities.find((c) => c.id === id) ?? null;
		createOpen = true;
	}

	async function onDeleteCapacity(id: string) {
		const cap = capacities.find((c) => c.id === id);
		const label = cap ? cap.display_name || cap.path : 'this capacity';
		if (!confirm(`Delete capacity “${label}”? Its backing net (if any) is retired.`)) return;
		try {
			await deleteResource(id);
			await poll();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to delete capacity';
		}
	}

	/** Scheduler card → force the cluster session to reconnect. */
	async function onReconnectCapacity(id: string) {
		try {
			await reconnectCluster(id);
			await poll();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Reconnect failed';
		}
	}

	/** Scheduler card → drain: refuse new leases while held ones finish. */
	async function onDrainCapacity(id: string) {
		try {
			await drainCluster(id);
			await poll();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Drain failed';
		}
	}
</script>

<PageShell width="wide" testid="control-plane-page">
	<PageHeader
		title="Control Plane"
		subtitle="The runners (the actual nodes) that pick up work, and the dispatch capacities that back them — presence-driven runner groups, pull worker pools, seeded concurrency limits, and scheduler clusters."
	>
		{#snippet actions()}
			{#if activeTab === 'capacities'}
				<Button
					variant="default"
					size="sm"
					class="gap-1.5"
					onclick={openCreate}
					data-testid="new-capacity-button"
				>
					<Plus class="size-4" />
					New capacity
				</Button>
			{/if}
		{/snippet}
	</PageHeader>

	<Tabs.Root
		value={activeTab}
		onValueChange={(v) => (activeTab = (v as 'runners' | 'capacities') ?? 'runners')}
		class="mb-6"
	>
		<Tabs.List>
			<Tabs.Trigger value="runners" data-testid="cp-tab-runners">Runners</Tabs.Trigger>
			<Tabs.Trigger value="capacities" data-testid="cp-tab-capacities">Capacities</Tabs.Trigger>
		</Tabs.List>
	</Tabs.Root>

	{#if activeTab === 'runners'}
		<!-- The runner roster IS the unified machine view; Engines is this list
			 filtered to model servers (role=engines). RunnerList owns its own
			 enroll + token management. -->
		<RunnerList role={roleParam} />
	{:else}
		<div class="mb-6">
			<BoardHeader title="Capacities" {summary} updated={lastUpdated} />
		</div>

		{#if error}
			<div
				class="mb-6 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200"
			>
				{error}
			</div>
		{/if}

		<div class="space-y-10">
			<!-- PRESENCE — runner groups -->
			<CapacitySection
				title="Presence"
				backend="presence"
				capacities={presence}
				emptyMessage="No runner groups. Create a runner group, then enroll a runner into it."
				onedit={onEditCapacity}
				ondelete={onDeleteCapacity}
				onenroll={onEnrollCapacity}
			>
				{#snippet emptyIcon()}<Server class="size-10 text-muted-foreground/40" />{/snippet}
				{#snippet action()}
					<Button
						variant="outline"
						size="sm"
						class="gap-1.5"
						onclick={openEnroll}
						data-testid="enroll-runner-button"
					>
						<UserPlus class="size-4" />
						Enroll runner
					</Button>
				{/snippet}
			</CapacitySection>

			<!-- QUEUE — worker groups. Backend coverage is a PER-GROUP attribute
				 (each card shows its group's served backends), so there is no
				 fleet-wide coverage blob here. -->
			<CapacitySection
				title="Queue"
				backend="queue"
				capacities={queue}
				emptyMessage="No worker groups."
				onedit={onEditCapacity}
				ondelete={onDeleteCapacity}
				onenroll={onEnrollWorkerCapacity}
			>
				{#snippet emptyIcon()}<Cpu class="size-10 text-muted-foreground/40" />{/snippet}
				{#snippet action()}
					<Button
						variant="outline"
						size="sm"
						class="gap-1.5"
						onclick={openEnrollWorker}
						data-testid="enroll-worker-button"
					>
						<UserPlus class="size-4" />
						Enroll worker
					</Button>
				{/snippet}
			</CapacitySection>

			<!-- TOKENS — concurrency limits -->
			<CapacitySection
				title="Tokens"
				backend="tokens"
				capacities={tokens}
				emptyMessage="No concurrency limits."
				onedit={onEditCapacity}
				ondelete={onDeleteCapacity}
			>
				{#snippet emptyIcon()}<KeyRound class="size-10 text-muted-foreground/40" />{/snippet}
			</CapacitySection>

			<!-- SCHEDULER — clusters / datacenters -->
			<CapacitySection
				title="Scheduler"
				backend="scheduler"
				capacities={scheduler}
				emptyMessage="No scheduler clusters."
				onedit={onEditCapacity}
				ondelete={onDeleteCapacity}
				onreconnect={onReconnectCapacity}
				ondrain={onDrainCapacity}
			>
				{#snippet emptyIcon()}<Boxes class="size-10 text-muted-foreground/40" />{/snippet}
			</CapacitySection>

			<!-- HUMANS — offer-dispatch presence pools (docs/33). Roster members +
				 their live presence; the human counterpart to the runner cards. -->
			<HumanCapacitySection capacities={humans} />

			<!-- The self-hosted LLM model pool (engines, catalog, curated set,
				 placement, router/inference-audit) now lives on its own page at
				 /models — see the "Models" top-nav entry. -->
		</div>
	{/if}
</PageShell>

<!-- Capacity create/edit flow: the dedicated kind-switcher modal. `editing`
	 null ⇒ create; a summary ⇒ edit (kind + name locked, fields prefilled). -->
<NewCapacityModal bind:open={createOpen} {editing} onsaved={onCreated} />

<!-- Enroll flow: global (group picker) or scoped to a presence group's path. -->
<EnrollSheet bind:open={enrollOpen} group={enrollGroup} onenrolled={() => void poll()} />

<!-- Worker enroll flow: global (workers-group picker) or scoped to a queue group's path. -->
<EnrollSheet
	bind:open={enrollWorkerOpen}
	mode="worker"
	group={enrollWorkerGroup}
	onenrolled={() => void poll()}
/>
