<script lang="ts">
	// The unified Control Plane. Its top-level sections ARE the four dispatch
	// backends — PRESENCE (runner groups), QUEUE (worker pools), TOKENS
	// (concurrency limits), SCHEDULER (clusters/datacenters) — mirroring
	// `CapacityAxes::backend()` 1:1. Each capacity is a compact card under its
	// backend with live numbers, read off `GET /api/v1/capacities` (polled ~5s).
	//
	// Page-level actions wire to the dedicated Control-Plane flows:
	//   "New capacity"  → NewCapacityModal (kind switcher: runner group / limit /
	//                      worker / cluster).
	//   "Enroll runner" → EnrollSheet, no fixed group (global flow w/ group picker).
	//   card "Enroll here" → EnrollSheet, fixed to that presence group's path.
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
	import CapacitySection from '$lib/components/fleet/CapacitySection.svelte';
	import BoardHeader from '$lib/components/fleet/BoardHeader.svelte';
	import NewCapacityModal from '$lib/components/fleet/NewCapacityModal.svelte';
	import EnrollSheet from '$lib/components/fleet/EnrollSheet.svelte';

	// ── State ──────────────────────────────────────────────────────────────────

	let capacities = $state<CapacitySummary[]>([]);
	let error = $state<string | null>(null);
	let lastUpdated = $state<Date | null>(null);

	// "New capacity" → the dedicated kind-switcher modal. `editing` non-null ⇒ the
	// same modal in edit mode (kind + name locked, fields prefilled).
	let createOpen = $state(false);
	let editing = $state<CapacitySummary | null>(null);
	// "Enroll runner" → EnrollSheet. `enrollGroup` null ⇒ global (picker); a path
	// ⇒ scoped to that presence group (per-card "Enroll here").
	let enrollOpen = $state(false);
	let enrollGroup = $state<string | null>(null);

	// ── Derived: partition by backend ───────────────────────────────────────────

	const presence = $derived(capacities.filter((c) => c.backend === 'presence'));
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

<svelte:head><title>Control Plane | Mekhan</title></svelte:head>

<div class="h-full overflow-y-auto" data-testid="control-plane-page">
	<div class="mx-auto max-w-6xl px-6 py-8 animate-rise">
		<div class="mb-6 flex items-start justify-between gap-4">
			<div>
				<h1 class="text-2xl font-semibold tracking-tight text-foreground">Control Plane</h1>
				<p class="mt-1 text-sm text-muted-foreground">
					Every capacity the platform can dispatch work to, grouped by backend: presence-driven
					runner groups, pull worker pools, seeded concurrency limits, and scheduler clusters.
				</p>
			</div>
			<Button
				variant="default"
				size="sm"
				class="shrink-0 gap-1.5"
				onclick={openCreate}
				data-testid="new-capacity-button"
			>
				<Plus class="size-4" />
				New capacity
			</Button>
		</div>

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

			<!-- QUEUE — worker pools -->
			<CapacitySection
				title="Queue"
				backend="queue"
				capacities={queue}
				emptyMessage="No worker pools."
				onedit={onEditCapacity}
				ondelete={onDeleteCapacity}
			>
				{#snippet emptyIcon()}<Cpu class="size-10 text-muted-foreground/40" />{/snippet}
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
		</div>
	</div>
</div>

<!-- Capacity create/edit flow: the dedicated kind-switcher modal. `editing`
	 null ⇒ create; a summary ⇒ edit (kind + name locked, fields prefilled). -->
<NewCapacityModal bind:open={createOpen} {editing} onsaved={onCreated} />

<!-- Enroll flow: global (group picker) or scoped to a presence group's path. -->
<EnrollSheet bind:open={enrollOpen} group={enrollGroup} onenrolled={() => void poll()} />
