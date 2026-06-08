<script lang="ts">
	// Fleet → Runner list, split into its groups.
	// On mount: load listRunners() + getRunnerPresence() + the presence `capacity`
	// resources (the runner groups), then groupFleet() into sections (one per
	// backed group, then any unbacked aliases, then ungrouped) — rendered via the
	// shared GroupSectionHeader (same split as the Live board). A runner's `group`
	// is only meaningful when backed by a presence `capacity` resource — that
	// resource carries the presence-pool net the runner's unit is admitted into —
	// so the enroll dialog only lets you pick an EXISTING group (or create one
	// inline first). A runner group is a `capacity` resource created from the
	// `instrument` preset (liveness=presence).
	import { untrack } from 'svelte';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription,
		SheetClose
	} from '$lib/components/ui/sheet';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import * as Select from '$lib/components/ui/select';
	import { toast } from 'svelte-sonner';
	import Server from '@lucide/svelte/icons/server';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import Info from '@lucide/svelte/icons/info';
	import ArrowUpRight from '@lucide/svelte/icons/arrow-up-right';
	import {
		listRunners,
		getRunner,
		getRunnerPresence,
		revokeRunner,
		listRegistrationTokens,
		revokeRegistrationToken,
		type RunnerSummary,
		type RunnerDetail,
		type RunnerPresenceSnapshot,
		type RegistrationTokenSummary
	} from '$lib/api/runners';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import { listFleetEngines } from '$lib/api/models';
	import { capacityTarget } from '$lib/editor/deployment-run-target';
	import { groupFleet, filterFleetByGroup, type FleetSection } from './grouping';
	import { accelLabel, hostSummary, runnerRole, isStale } from './runner-identity';
	import { fmtMsAgo, fmtDate } from './format';
	import StatusDot from './StatusDot.svelte';
	import BackendChips from './BackendChips.svelte';
	import GroupSectionHeader from './GroupSectionHeader.svelte';
	import CoverageStrip from './CoverageStrip.svelte';
	import FleetEmpty from './FleetEmpty.svelte';
	import EnrollSheet from './EnrollSheet.svelte';
	import Cpu from '@lucide/svelte/icons/cpu';

	type Props = {
		/** When set, scope the list to this group alias (the capacity `path`):
		 *  only its runners + tokens, no cross-group sections. Omitted ⇒ the full
		 *  fleet split into sections (the Control Plane's runner-management panel). */
		group?: string | null;
		/** Roster mode: a per-group view (used by /fleet/[id]). Hides the
		 *  group-section chrome (there is only one group), adds the per-group
		 *  coverage strip + pool-net link + freshness, and delegates Enroll to the
		 *  page header (the single enroll action) via `onenroll`. */
		roster?: boolean;
		/** Roster-mode enroll handler — invoked instead of the in-component sheet so
		 *  the page header's "Enroll here" is the one and only enroll affordance. */
		onenroll?: () => void;
		/** Initial role filter. 'engines' scopes the roster to model-serving runners
		 *  (the Engines facet); the operator can still flip it back to 'all'. */
		role?: 'all' | 'engines';
	};
	let { group = null, roster = false, onenroll, role = 'all' }: Props = $props();

	// ── State ──────────────────────────────────────────────────────────────────

	let runners = $state<RunnerSummary[]>([]);
	let presence = $state<RunnerPresenceSnapshot[]>([]);
	let groups = $state<ResourceSummary[]>([]);
	let tokens = $state<RegistrationTokenSummary[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let lastUpdated = $state<Date | null>(null);

	// Filters
	let onlineFilter = $state<'all' | 'online'>('all');
	// Seed the (operator-toggleable) role filter from the prop's INITIAL value
	// only — `untrack` documents that intent and keeps it out of the reactive read.
	let roleFilter = $state<'all' | 'engines'>(untrack(() => role));

	// runner_id → resident model engine base ids (the model-serving facet, joined
	// from GET /api/v1/fleet/engines). Fail-soft: an engines fetch error leaves
	// this empty and the roster simply shows no model chips.
	let enginesByRunner = $state<Record<string, string[]>>({});

	// Revoke
	let revoking = $state<string | null>(null);
	let revokingToken = $state<string | null>(null);

	// Enroll sheet (mint + reveal-once now lives in EnrollSheet). In roster mode
	// the sheet is scoped to this group; otherwise it shows a group picker.
	let enrollOpen = $state(false);

	// Detail drawer (full record incl. capabilities + nats_public_key)
	let detail = $state<RunnerDetail | null>(null);
	let detailLoading = $state(false);
	let detailError = $state<string | null>(null);

	// ── Derived ────────────────────────────────────────────────────────────────

	/** Fast lookup: runner_id → presence snapshot */
	const presenceById = $derived(Object.fromEntries(presence.map((p) => [p.runner_id, p])));

	/** The fleet split into ordered sections (backed → unbacked → ungrouped). */
	const sections = $derived(groupFleet(runners, presenceById, groups));

	/** Runner ids that are real dispatch targets — i.e. in a BACKED group (their
	 *  group has a presence `capacity` resource, so their unit is admitted into a
	 *  pool with C slots). A runner NOT in this set is "pool-less": it heartbeats
	 *  and advertises backends, but `runners_presence` admits it to no pool
	 *  (`concurrency = 0`), so it dispatches NOTHING. A model server is the common
	 *  case — it's intentionally ungrouped because inference goes over HTTP, not
	 *  presence dispatch. Surfacing this stops the roster implying a pool-less node
	 *  serves the (inert) backends its fat executor binary happens to advertise. */
	const dispatchTargetIds = $derived(
		new Set(
			sections.filter((s) => s.kind === 'backed').flatMap((s) => s.runners.map((r) => r.id))
		)
	);
	const isPoolLess = (id: string): boolean => !dispatchTargetIds.has(id);

	/** The backing group resource (for the pool-net deep-link), roster mode only. */
	const groupResource = $derived<ResourceSummary | null>(groups[0] ?? null);

	/** Per-backend coverage for THIS group: how many present runners advertise each
	 *  backend — the strip that used to live on the (now-folded-in) Board. */
	const coverage = $derived.by<{ backend: string; count: number }[]>(() => {
		const counts = new Map<string, number>();
		for (const r of runners) {
			const snap = presenceById[r.id];
			if (!snap?.present) continue;
			for (const be of snap.backends ?? []) counts.set(be, (counts.get(be) ?? 0) + 1);
		}
		return [...counts.entries()]
			.map(([backend, count]) => ({ backend, count }))
			.sort((a, b) => a.backend.localeCompare(b.backend));
	});

	/** A runner's live backends (only meaningful while present). */
	function liveBackends(id: string): string[] {
		const snap = presenceById[id];
		return snap?.present ? (snap.backends ?? []) : [];
	}

	/** Resident model engine base ids for a runner (empty unless it serves models). */
	function residentModels(id: string): string[] {
		return enginesByRunner[id] ?? [];
	}

	/** Whether a runner serves model engines — the Engines facet membership. */
	function isModelServer(id: string): boolean {
		return (enginesByRunner[id]?.length ?? 0) > 0;
	}

	/** Apply the online + role filters to a section's runners for display. */
	function shown(section: FleetSection): RunnerSummary[] {
		let rows = section.runners;
		if (onlineFilter === 'online') rows = rows.filter((r) => presenceById[r.id]?.present);
		if (roleFilter === 'engines') rows = rows.filter((r) => isModelServer(r.id));
		return rows;
	}

	// ── Load ───────────────────────────────────────────────────────────────────

	async function load(silent = false) {
		if (!silent) loading = true;
		error = null;
		try {
			const [rPage, pSnaps, gPage, tPage, engResult] = await Promise.all([
				listRunners({ perPage: 200 }),
				getRunnerPresence(),
				listResources({ resource_type: 'capacity', perPage: 200 }),
				listRegistrationTokens({ perPage: 200 }),
				// Fail-soft: the engines join only adds model chips — never let it wipe
				// the roster, so swallow its error into an empty inventory.
				listFleetEngines().catch(() => ({ headroom_from_router: false, nodes: [] }))
			]);
			presence = pSnaps;
			enginesByRunner = Object.fromEntries(
				engResult.nodes.map((n) => [n.runner_id, n.engines.map((e) => e.base)])
			);
			// A runner group is a presence `capacity` (the instrument preset);
			// other capacity flavours (seeded limits, worker queues) are not groups.
			const allGroups = gPage.items.filter((r) => capacityTarget(r) === 'runner_group');
			// When scoped to one group, drop everything else (runners, backing res,
			// and tokens for other groups).
			const filtered = filterFleetByGroup(rPage.items, allGroups, group);
			runners = filtered.runners;
			groups = filtered.groupResources;
			tokens = group == null ? tPage.items : tPage.items.filter((t) => t.group === group);
			lastUpdated = new Date();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load runners';
			runners = [];
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void load();
		// Roster (per-group detail) refreshes presence live, like the old Board did.
		if (!roster) return;
		const t = setInterval(() => void load(true), 5000);
		return () => clearInterval(t);
	});

	// ── Actions ────────────────────────────────────────────────────────────────

	async function handleRevoke(runner: RunnerSummary) {
		if (revoking) return;
		if (!confirm(`Revoke runner "${runner.name}"? It will be unable to pick up new jobs.`)) return;
		revoking = runner.id;
		try {
			await revokeRunner(runner.id);
			toast.success('Runner revoked.');
			await load();
		} catch (e) {
			toast.error(`Revoke failed: ${e instanceof Error ? e.message : e}`);
		} finally {
			revoking = null;
		}
	}

	async function handleRevokeToken(token: RegistrationTokenSummary) {
		if (revokingToken) return;
		if (!confirm("Revoke this registration token? Runners that haven't enrolled yet won't be able to use it.")) return;
		revokingToken = token.id;
		try {
			await revokeRegistrationToken(token.id);
			toast.success('Token revoked.');
			await load();
		} catch (e) {
			toast.error(`Revoke failed: ${e instanceof Error ? e.message : e}`);
		} finally {
			revokingToken = null;
		}
	}

	/** Back an UNBACKED group: create its presence `capacity` resource (the
	    `instrument` preset deploys the pool net), upgrading present-but-unadmitted
	    runners on the next heartbeat. */
	// ── Formatting helpers ─────────────────────────────────────────────────────

	/** Produce a compact key=value capability summary string. */
	function capsSummary(caps: Record<string, unknown> | null | undefined): string {
		if (!caps) return '—';
		const keys = Object.keys(caps);
		if (keys.length === 0) return '—';
		return keys
			.slice(0, 4)
			.map((k) => `${k}=${JSON.stringify(caps[k])}`)
			.join(', ')
			.concat(keys.length > 4 ? ` +${keys.length - 4}` : '');
	}

	/** Open the detail drawer for a runner and fetch its full record. */
	async function openDetail(id: string) {
		detail = null;
		detailError = null;
		detailLoading = true;
		// A non-null sentinel keeps the sheet open while the fetch is in flight.
		detail = { id } as RunnerDetail;
		try {
			detail = await getRunner(id);
		} catch (e) {
			detailError = e instanceof Error ? e.message : String(e);
		} finally {
			detailLoading = false;
		}
	}

	/** Pretty-print a capabilities object for the detail drawer. */
	function capsPretty(caps: unknown): string {
		try {
			return JSON.stringify(caps ?? {}, null, 2);
		} catch {
			return String(caps);
		}
	}

	function openEnroll() {
		// Roster mode defers to the page header's single "Enroll here" action; the
		// full-fleet mode owns its own sheet.
		if (roster) onenroll?.();
		else enrollOpen = true;
	}
</script>

<!-- ── Toolbar ──────────────────────────────────────────────────────────────── -->
<div class="space-y-4" data-testid="runner-list">
	{#snippet statusFilter()}
		<div class="flex items-center gap-2">
			<span class="text-sm font-medium text-muted-foreground">Status</span>
			<Select.Root
				type="single"
				value={onlineFilter}
				onValueChange={(v) => (onlineFilter = (v as 'all' | 'online') ?? 'all')}
			>
				<Select.Trigger class="h-9 min-w-[120px]">
					{onlineFilter === 'online' ? 'Online only' : 'All'}
				</Select.Trigger>
				<Select.Content>
					<Select.Item value="all" label="All" />
					<Select.Item value="online" label="Online only" />
				</Select.Content>
			</Select.Root>
		</div>
	{/snippet}

	{#snippet roleSelect()}
		<div class="flex items-center gap-2">
			<span class="text-sm font-medium text-muted-foreground">Role</span>
			<Select.Root
				type="single"
				value={roleFilter}
				onValueChange={(v) => (roleFilter = (v as 'all' | 'engines') ?? 'all')}
			>
				<Select.Trigger class="h-9 min-w-[140px]" data-testid="role-filter">
					{roleFilter === 'engines' ? 'Model servers' : 'All roles'}
				</Select.Trigger>
				<Select.Content>
					<Select.Item value="all" label="All roles" />
					<Select.Item value="engines" label="Model servers" />
				</Select.Content>
			</Select.Root>
		</div>
	{/snippet}

	{#if roster}
		<!-- Info band: per-group backend coverage + poll freshness (the group name /
			 online count already live in the page header). -->
		{#if coverage.length > 0 || lastUpdated}
			<div class="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
				<CoverageStrip entries={coverage} />
				{#if lastUpdated}
					<span class="text-sm tabular-nums text-muted-foreground"
						>updated {lastUpdated.toLocaleTimeString()}</span
					>
				{/if}
			</div>
		{/if}

		<!-- Runner-cards header row: title + status filter on the left, the two
			 actions (View pool net · Enroll here) on the right. -->
		<div class="flex flex-wrap items-center justify-between gap-3 border-b border-border pb-3">
			<div class="flex items-center gap-3">
				<h3 class="text-sm font-semibold text-foreground">Runners</h3>
				{@render statusFilter()}
			</div>
			<div class="flex items-center gap-2">
				{#if groupResource}
					<Button
						href="/nets/pool-{groupResource.id}"
						variant="outline"
						size="sm"
						class="gap-1.5"
						data-testid="view-pool-net"
					>
						<ArrowUpRight class="size-4" />
						View pool net
					</Button>
				{/if}
				<Button
					variant="default"
					size="sm"
					onclick={openEnroll}
					class="gap-1.5"
					data-testid="runner-enroll-button"
				>
					<Plus class="size-4" />
					Enroll here
				</Button>
			</div>
		</div>
	{:else}
		<div class="flex flex-wrap items-center gap-3">
			{@render statusFilter()}
			{@render roleSelect()}
			<Button
				variant="default"
				size="sm"
				onclick={openEnroll}
				class="ml-auto gap-1.5"
				data-testid="runner-enroll-button"
			>
				<Plus class="size-4" />
				New runner
			</Button>
		</div>
	{/if}

	<!-- ── Error ──────────────────────────────────────────────────────────────── -->
	{#if error}
		<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{/if}

	<!-- ── Sectioned runner list (one block per group) ──────────────────────────── -->
	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">Loading…</div>
	{:else if runners.length === 0 && groups.length === 0}
		<FleetEmpty message="No runners enrolled yet.">
			{#snippet icon()}<Server class="size-10 text-muted-foreground/40" />{/snippet}
			{#snippet action()}
				<Button variant="outline" size="sm" class="gap-1.5" onclick={openEnroll}>
					<Plus class="size-4" />
					Enroll your first runner
				</Button>
			{/snippet}
		</FleetEmpty>
	{:else}
		<div class="space-y-6">
			{#each sections as section (section.kind + ':' + (section.alias ?? '∅'))}
				{@const rows = shown(section)}
				<section data-testid="group-section-{section.alias ?? 'ungrouped'}">
					{#if !roster}
						<GroupSectionHeader {section} />
					{/if}

					<!-- Section rows -->
					{#if rows.length === 0}
						<p class="px-1 py-2 text-sm text-muted-foreground">
							{#if section.runners.length === 0}
								No runners enrolled in this group yet.
							{:else}
								No online runners in this group.
							{/if}
						</p>
					{:else}
						<div class="space-y-2">
							{#each rows as runner (runner.id)}
								{@const snap = presenceById[runner.id]}
								{@const online = snap?.present ?? false}
								{@const role = runnerRole(
									runner.capabilities as Record<string, unknown>,
									isModelServer(runner.id)
								)}
								{@const caps = capsSummary(runner.capabilities as Record<string, unknown>)}
								{@const poolLess = isPoolLess(runner.id)}
								<div
									class="group flex items-center justify-between rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent/40"
									data-testid="runner-item-{runner.id}"
								>
									<div class="flex min-w-0 flex-1 items-start gap-3">
										<!-- Online dot -->
										<Tooltip.Provider>
											<Tooltip.Root>
												<Tooltip.Trigger>
													<StatusDot tone={online ? 'live' : 'idle'} class="mt-1" />
												</Tooltip.Trigger>
												<Tooltip.Content>
													{#if online && snap}
														Online · last heartbeat {fmtMsAgo(snap.last_seen_ms_ago)}
													{:else}
														Offline · last seen {fmtDate(runner.last_seen_at)}
													{/if}
												</Tooltip.Content>
											</Tooltip.Root>
										</Tooltip.Provider>

										<!-- Detail -->
										<div class="min-w-0 flex-1">
											<div class="flex flex-wrap items-center gap-2">
												<span class="text-sm font-medium text-foreground">{runner.name}</span>
												{#if role.key !== 'generic'}
													<Badge variant="secondary" class="text-sm">{role.label}</Badge>
												{/if}
												<Badge variant="outline" class="text-sm">{runner.status}</Badge>
												{#if isStale(snap)}
													<Badge
														variant="outline"
														class="border-amber-500/50 text-xs text-amber-700 dark:text-amber-400"
														title="Online but reporting no host — restart this runner on a current build to de-anonymise it."
													>
														stale build
													</Badge>
												{/if}
											</div>

											<!-- Host fingerprint one-liner (accelerator · hostname · IP); falls
												 back to the short id when the runner reports no host. -->
											{#if hostSummary(snap?.host)}
												<p class="mt-1 truncate font-mono text-sm text-muted-foreground">
													{hostSummary(snap?.host)}
												</p>
											{:else}
												<p class="mt-1 truncate font-mono text-sm text-muted-foreground/70">
													{runner.id}
												</p>
											{/if}

											<!-- Dispatch backends — only meaningful for a real dispatch target
												 (a runner admitted into a backed pool). A pool-less runner
												 advertises these but admits to no pool, so showing them would
												 imply a capability it doesn't actually serve. -->
											{#if online && !poolLess && liveBackends(runner.id).length > 0}
												<div class="mt-1.5">
													<BackendChips backends={liveBackends(runner.id)} />
												</div>
											{/if}

											<!-- Resident model engines — the model-serving facet (Engines lens).
												 Inference is served over HTTP, so this is live regardless of pool. -->
											{#if residentModels(runner.id).length > 0}
												<p
													class="mt-1 flex items-center gap-1.5 truncate text-sm text-muted-foreground"
												>
													<Cpu class="size-3.5 shrink-0" />
													<span class="font-mono">{residentModels(runner.id).join(' · ')}</span>
												</p>
											{/if}

											<!-- Honesty line for a pool-less runner: it dispatches nothing. A
												 model server serves inference over HTTP (the router calls it
												 directly); a plain ungrouped runner is just dangling. -->
											{#if poolLess && online}
												<p class="mt-1 text-sm text-muted-foreground/80">
													{#if role.key === 'model'}
														Serves inference over HTTP · not a dispatch target (no pool)
													{:else}
														Advertises backends but admitted to no pool · not a dispatch
														target
													{/if}
												</p>
											{/if}

											{#if caps !== '—'}
												<p class="mt-0.5 truncate text-sm text-muted-foreground">
													Caps: <span class="font-mono">{caps}</span>
												</p>
											{/if}
											<p class="mt-0.5 truncate text-sm text-muted-foreground">
												Enrolled {fmtDate(runner.enrolled_at)}
											</p>
										</div>
									</div>

									<!-- Hover actions -->
									<div
										class="flex shrink-0 items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100"
									>
										<Button
											variant="ghost"
											size="sm"
											class="text-muted-foreground"
											onclick={() => openDetail(runner.id)}
											title="Runner details"
										>
											<Info class="size-3.5" />
											Details
										</Button>
										<Button
											variant="ghost"
											size="sm"
											class="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
											onclick={() => handleRevoke(runner)}
											disabled={revoking === runner.id}
											title="Revoke runner"
										>
											<Trash2 class="size-3.5" />
											{revoking === runner.id ? 'Revoking…' : 'Revoke'}
										</Button>
									</div>
								</div>
							{/each}
						</div>
					{/if}
				</section>
			{/each}
		</div>
	{/if}

	<!-- ── Registration tokens sub-list ────────────────────────────────────────── -->
	{#if tokens.length > 0}
		<div class="pt-4">
			<h3 class="mb-2 text-sm font-medium text-muted-foreground">Registration tokens</h3>
			<div class="space-y-2">
				{#each tokens as token (token.id)}
					<div
						class="group flex items-center justify-between rounded-lg border border-border bg-card px-4 py-3 transition-colors hover:bg-accent/40"
					>
						<div class="min-w-0 space-y-0.5">
							<div class="flex flex-wrap items-center gap-2">
								{#if token.group}
									<Badge variant="secondary" class="text-sm">{token.group}</Badge>
								{/if}
								<Badge variant="outline" class="text-sm">
									{token.reusable ? 'reusable' : `1-shot · ${token.uses} used`}
								</Badge>
								{#if token.max_uses}
									<span class="text-sm text-muted-foreground">max {token.max_uses}</span>
								{/if}
							</div>
							<p class="text-sm text-muted-foreground">
								Created {fmtDate(token.created_at)}
								{#if token.expires_at}· Expires {fmtDate(token.expires_at)}{/if}
							</p>
						</div>
						<Button
							variant="ghost"
							size="sm"
							class="opacity-0 transition-opacity group-hover:opacity-100 text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
							onclick={() => handleRevokeToken(token)}
							disabled={revokingToken === token.id}
						>
							<Trash2 class="size-3.5" />
							{revokingToken === token.id ? 'Revoking…' : 'Revoke'}
						</Button>
					</div>
				{/each}
			</div>
		</div>
	{/if}
</div>

<!-- ── Enroll sheet (mint + reveal-once) — full-fleet mode only; roster defers to
	 the page header's single "Enroll here". ───────────────────────────────── -->
{#if !roster}
	<EnrollSheet bind:open={enrollOpen} group={null} onenrolled={load} />
{/if}

<!-- ── Runner detail drawer ────────────────────────────────────────────────── -->
<Sheet.Root
	open={detail !== null}
	onOpenChange={(o: boolean) => {
		if (!o) {
			detail = null;
			detailError = null;
		}
	}}
>
	<SheetContent class="w-[520px] overflow-y-auto sm:max-w-[520px]">
		<div class="space-y-4 p-2">
			<div class="space-y-1">
				<SheetTitle class="flex items-center gap-2 text-lg font-semibold">
					<Server class="size-4" />
					{detail?.name ?? 'Runner'}
				</SheetTitle>
				<SheetDescription class="font-mono text-sm">{detail?.id}</SheetDescription>
			</div>

			{#if detailError}
				<div
					class="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/10 p-3 text-sm text-destructive"
				>
					<TriangleAlert class="mt-0.5 size-4 shrink-0" />
					<span>{detailError}</span>
				</div>
			{:else if detailLoading}
				<p class="text-sm text-muted-foreground">Loading…</p>
			{:else if detail}
				{@const snap = presenceById[detail.id]}
				<dl class="grid grid-cols-3 gap-x-3 gap-y-2 text-sm">
					<dt class="text-muted-foreground">Status</dt>
					<dd class="col-span-2"><Badge variant="outline" class="text-sm">{detail.status}</Badge></dd>

					<dt class="text-muted-foreground">Online</dt>
					<dd class="col-span-2">
						{#if snap?.present}
							<span class="text-emerald-600">● online · {fmtMsAgo(snap.last_seen_ms_ago)}</span>
						{:else}
							<span class="text-muted-foreground">○ offline</span>
						{/if}
					</dd>

					<dt class="text-muted-foreground">Group</dt>
					<dd class="col-span-2">
						{#if detail.group}<Badge variant="secondary" class="text-sm">{detail.group}</Badge
							>{:else}—{/if}
					</dd>

					<dt class="text-muted-foreground">Backends</dt>
					<dd class="col-span-2 space-y-1">
						<BackendChips
							backends={snap?.present ? (snap.backends ?? []) : []}
							empty={snap?.present ? '—' : 'offline (advertised on connect)'}
						/>
						{#if isPoolLess(detail.id)}
							<p class="text-xs text-muted-foreground/80">
								Advertised, but this runner is admitted to no pool — these dispatch to
								nothing.{#if isModelServer(detail.id)}
									It serves model inference over HTTP instead.{/if}
							</p>
						{/if}
					</dd>

					<dt class="text-muted-foreground">Last seen</dt>
					<dd class="col-span-2">{fmtDate(detail.last_seen_at)}</dd>

					<dt class="text-muted-foreground">Enrolled</dt>
					<dd class="col-span-2">{fmtDate(detail.enrolled_at)}</dd>

					{#if detail.nats_public_key}
						<dt class="text-muted-foreground">NATS key</dt>
						<dd class="col-span-2 break-all font-mono text-sm">{detail.nats_public_key}</dd>
					{/if}
				</dl>

				{#if snap?.host}
					{@const h = snap.host}
					<div class="space-y-1">
						<h3 class="text-sm font-medium text-muted-foreground">Host</h3>
						<dl class="grid grid-cols-3 gap-x-3 gap-y-1.5 text-sm">
							{#if h.hostname}
								<dt class="text-muted-foreground">Hostname</dt>
								<dd class="col-span-2 break-all font-mono">{h.hostname}</dd>
							{/if}
							{#if h.os || h.arch}
								<dt class="text-muted-foreground">Platform</dt>
								<dd class="col-span-2">{[h.os, h.arch].filter(Boolean).join(' · ')}</dd>
							{/if}
							{#if h.cpu_cores || h.mem_gb}
								<dt class="text-muted-foreground">CPU / RAM</dt>
								<dd class="col-span-2">
									{[h.cpu_cores ? `${h.cpu_cores} cores` : null, h.mem_gb ? `${h.mem_gb} GB` : null]
										.filter(Boolean)
										.join(' · ')}
								</dd>
							{/if}
							{#if h.accelerator}
								<dt class="text-muted-foreground">Accelerator</dt>
								<dd class="col-span-2">{accelLabel(h)}</dd>
							{/if}
							{#if h.ips && h.ips.length > 0}
								<dt class="text-muted-foreground">IP</dt>
								<dd class="col-span-2 break-all font-mono">{h.ips.join(', ')}</dd>
							{/if}
						</dl>
					</div>
				{/if}

				<div class="space-y-1">
					<h3 class="text-sm font-medium text-muted-foreground">Capabilities</h3>
					<pre
						class="max-h-72 overflow-auto rounded-md border border-border bg-muted/40 p-3 font-mono text-sm">{capsPretty(
							detail.capabilities
						)}</pre>
				</div>
			{/if}

			<SheetClose>
				<Button variant="outline" class="w-full">Close</Button>
			</SheetClose>
		</div>
	</SheetContent>
</Sheet.Root>
