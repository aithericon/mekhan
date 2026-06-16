<script lang="ts">
	// Fleet → Machines: the flat physical inventory of every enrolled daemon —
	// runners AND workers — identity-first, with NO group sections. Pools appear
	// only as linked chips per row (group → /fleet/{capacity id}); the logical
	// pool view lives on the Pools tab. Two callers:
	//
	//   /fleet "Machines" tab — `group` null: full inventory + toolbar (status /
	//        kind / role filters, Tokens, Enroll dropdown).
	//   /fleet/[id] pool detail — `group` set: scoped to one pool path, slim
	//        count line only (the page band owns actions; `onenroll` forwards
	//        its "Enroll here").
	//
	// Runner liveness comes from the presence snapshot (the actual pool-capacity
	// signal); worker liveness is the server-computed `online` flag (derived from
	// mekhan's FleetLiveness presence snapshot — the executor's `worker.{id}.presence`
	// NATS heartbeat) plus a last_seen freshness line. Polls every 5s.
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
	import {
		DropdownMenu,
		DropdownMenuTrigger,
		DropdownMenuContent,
		DropdownMenuItem
	} from '$lib/components/ui/dropdown-menu';
	import { toast } from 'svelte-sonner';
	import Server from '@lucide/svelte/icons/server';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import Info from '@lucide/svelte/icons/info';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import Cpu from '@lucide/svelte/icons/cpu';
	import {
		listRunners,
		getRunner,
		getRunnerPresence,
		revokeRunner,
		type RunnerSummary,
		type RunnerDetail,
		type RunnerPresenceSnapshot
	} from '$lib/api/runners';
	import { listWorkers, type WorkerSummary } from '$lib/api/workers';
	import { listCapacities, type CapacitySummary } from '$lib/api/capacities';
	import { listFleetEngines } from '$lib/api/models';
	import { accelLabel, hostSummary, isStale } from './runner-identity';
	import { fmtMsAgo, fmtDate } from './format';
	import StatusDot from './StatusDot.svelte';
	import BackendChips from './BackendChips.svelte';
	import EnrollSheet from './EnrollSheet.svelte';
	import TokensSheet from './TokensSheet.svelte';

	type Props = {
		/** Initial role filter. 'engines' scopes the inventory to model-serving
		 *  machines; the operator can still flip it back to 'all'. */
		role?: 'all' | 'engines';
		/** When set, scope to ONE pool path (runners.group / workers.group ===
		 *  group): pool chips + kind filter hidden, no toolbar — the page band
		 *  owns actions and `onenroll` forwards its "Enroll here". */
		group?: string | null;
		/** Scoped-mode enroll handler — when provided (with `group` set), a small
		 *  "Enroll here" button forwards to the page's single enroll affordance. */
		onenroll?: () => void;
	};
	let { role = 'all', group = null, onenroll }: Props = $props();

	// ── State ──────────────────────────────────────────────────────────────────

	let runners = $state<RunnerSummary[]>([]);
	let workers = $state<WorkerSummary[]>([]);
	let presence = $state<RunnerPresenceSnapshot[]>([]);
	let capacities = $state<CapacitySummary[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Filters (toolbar; only rendered when `group` is null)
	let statusFilter = $state<'all' | 'online'>('all');
	let kindFilter = $state<'all' | 'runner' | 'worker'>('all');
	// Seed the (operator-toggleable) role filter from the prop's INITIAL value
	// only — `untrack` documents that intent and keeps it out of the reactive read.
	let roleFilter = $state<'all' | 'engines'>(untrack(() => role));

	// runner_id → resident / provisioned model engine base ids (the model-serving
	// facet, joined from GET /api/v1/fleet/engines). Fail-soft: an engines fetch
	// error leaves these empty and rows simply show no model chips.
	let enginesByRunner = $state<Record<string, string[]>>({});
	let pulledByRunner = $state<Record<string, string[]>>({});
	let modelServingIds = $state<Set<string>>(new Set());

	// Revoke (runners only — workers have no revoke API)
	let revoking = $state<string | null>(null);

	// Sheets
	let enrollOpen = $state(false);
	let enrollMode = $state<'runner' | 'worker'>('runner');
	let tokensOpen = $state(false);

	// Detail drawer — runner (full record) or worker (compact)
	let detail = $state<RunnerDetail | null>(null);
	let detailLoading = $state(false);
	let detailError = $state<string | null>(null);
	let workerDetail = $state<WorkerSummary | null>(null);

	// ── Row model ──────────────────────────────────────────────────────────────

	type MachineRow = {
		kind: 'runner' | 'worker';
		id: string;
		name: string;
		group: string | null;
		online: boolean;
		/** Freshness for the offline/never-seen tooltip + worker meta line. */
		lastSeen: string;
		backends: string[];
		enrolledAt: string;
		runner?: RunnerSummary;
		worker?: WorkerSummary;
		snap?: RunnerPresenceSnapshot;
	};

	/** Fast lookup: runner_id → presence snapshot */
	const presenceById = $derived(Object.fromEntries(presence.map((p) => [p.runner_id, p])));

	/** Pool path → capacity id, for the linked pool chips. */
	const capacityIdByPath = $derived(
		Object.fromEntries(capacities.map((c) => [c.path, c.id]))
	);

	/** A worker's advertised backends — `WorkerSummary.backends` is wire `unknown`. */
	function workerBackends(w: WorkerSummary): string[] {
		return Array.isArray(w.backends) ? (w.backends as string[]) : [];
	}

	/** Worker freshness off `last_seen_at` (ISO) — "3s ago" / "never seen". */
	function workerLastSeen(w: WorkerSummary): string {
		if (!w.last_seen_at) return 'never seen';
		const ms = Date.now() - new Date(w.last_seen_at).getTime();
		return Number.isNaN(ms) ? w.status : fmtMsAgo(ms);
	}

	/** The merged flat inventory: runners then workers, group-scoped when set. */
	const allRows = $derived.by<MachineRow[]>(() => {
		const rRows: MachineRow[] = runners.map((r) => {
			const snap = presenceById[r.id];
			return {
				kind: 'runner' as const,
				id: r.id,
				name: r.name,
				group: r.group ?? null,
				online: snap?.present ?? false,
				lastSeen: snap?.present ? fmtMsAgo(snap.last_seen_ms_ago) : fmtDate(r.last_seen_at),
				backends: snap?.present ? (snap.backends ?? []) : [],
				enrolledAt: r.enrolled_at,
				runner: r,
				snap
			};
		});
		const wRows: MachineRow[] = workers.map((w) => ({
			kind: 'worker' as const,
			id: w.id,
			name: w.name,
			group: w.group ?? null,
			// Liveness is the server-computed `online` flag (derived from the live
			// FleetLiveness presence snapshot), NOT `status` — `status` is the
			// lifecycle marker (enrolled/revoked) and is never 'online'.
			online: w.online,
			lastSeen: workerLastSeen(w),
			backends: workerBackends(w),
			enrolledAt: w.enrolled_at,
			worker: w
		}));
		const merged = [...rRows, ...wRows];
		return group != null ? merged.filter((m) => m.group === group) : merged;
	});

	/** Toolbar filters applied for display. */
	const rows = $derived.by<MachineRow[]>(() => {
		let shown = allRows;
		if (statusFilter === 'online') shown = shown.filter((m) => m.online);
		if (group == null && kindFilter !== 'all') shown = shown.filter((m) => m.kind === kindFilter);
		if (roleFilter === 'engines')
			shown = shown.filter((m) => m.kind === 'runner' && modelServingIds.has(m.id));
		return shown;
	});

	const onlineCount = $derived(allRows.filter((m) => m.online).length);

	/** Resident (currently-loaded) model engine base ids for a runner. */
	function residentModels(id: string): string[] {
		return enginesByRunner[id] ?? [];
	}

	/** Provisioned-but-not-loaded models on a runner ("ready to load"). */
	function readyModels(id: string): string[] {
		return pulledByRunner[id] ?? [];
	}

	// ── Load ───────────────────────────────────────────────────────────────────

	async function load(silent = false) {
		if (!silent) loading = true;
		error = null;
		try {
			const [rPage, wPage, pSnaps, caps, engResult] = await Promise.all([
				listRunners({ perPage: 200 }),
				listWorkers({ perPage: 200 }),
				getRunnerPresence(),
				listCapacities(),
				// Fail-soft: the engines join only adds model chips — never let it
				// wipe the inventory, so swallow its error into an empty inventory.
				listFleetEngines().catch(() => ({ headroom_from_router: false, nodes: [] }))
			]);
			runners = rPage.items;
			workers = wPage.items;
			presence = pSnaps;
			capacities = caps;
			// Resident (currently-loaded) engines drive the per-row model chips…
			enginesByRunner = Object.fromEntries(
				engResult.nodes.map((n) => [n.runner_id, n.engines.map((e) => e.base)])
			);
			// …while provisioned-but-not-loaded models are "ready to load".
			pulledByRunner = Object.fromEntries(
				engResult.nodes.map((n) => [n.runner_id, n.pulled ?? []])
			);
			// A runner is a MODEL SERVER if it serves OR has provisioned any model.
			modelServingIds = new Set(
				engResult.nodes
					.filter((n) => n.engines.length > 0 || (n.pulled?.length ?? 0) > 0)
					.map((n) => n.runner_id)
			);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load machines';
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void load();
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
			await load(true);
		} catch (e) {
			toast.error(`Revoke failed: ${e instanceof Error ? e.message : e}`);
		} finally {
			revoking = null;
		}
	}

	/** Open the detail drawer for a runner and fetch its full record. */
	async function openDetail(id: string) {
		workerDetail = null;
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

	function openEnroll(mode: 'runner' | 'worker') {
		enrollMode = mode;
		enrollOpen = true;
	}
</script>

<div class="space-y-4" data-testid="machines-table">
	<!-- ── Toolbar (full-inventory mode) / slim count line (scoped mode) ──────── -->
	{#if group == null}
		<div class="flex flex-wrap items-center gap-3">
			<div class="flex items-center gap-2">
				<span class="text-sm font-medium text-muted-foreground">Status</span>
				<Select.Root
					type="single"
					value={statusFilter}
					onValueChange={(v) => (statusFilter = (v as 'all' | 'online') ?? 'all')}
				>
					<Select.Trigger class="h-9 min-w-[120px]" data-testid="machines-status-filter">
						{statusFilter === 'online' ? 'Online only' : 'All'}
					</Select.Trigger>
					<Select.Content>
						<Select.Item value="all" label="All" />
						<Select.Item value="online" label="Online only" />
					</Select.Content>
				</Select.Root>
			</div>
			<div class="flex items-center gap-2">
				<span class="text-sm font-medium text-muted-foreground">Kind</span>
				<Select.Root
					type="single"
					value={kindFilter}
					onValueChange={(v) => (kindFilter = (v as 'all' | 'runner' | 'worker') ?? 'all')}
				>
					<Select.Trigger class="h-9 min-w-[120px]" data-testid="machines-kind-filter">
						{kindFilter === 'runner' ? 'Runners' : kindFilter === 'worker' ? 'Workers' : 'All'}
					</Select.Trigger>
					<Select.Content>
						<Select.Item value="all" label="All" />
						<Select.Item value="runner" label="Runners" />
						<Select.Item value="worker" label="Workers" />
					</Select.Content>
				</Select.Root>
			</div>
			<div class="flex items-center gap-2">
				<span class="text-sm font-medium text-muted-foreground">Role</span>
				<Select.Root
					type="single"
					value={roleFilter}
					onValueChange={(v) => (roleFilter = (v as 'all' | 'engines') ?? 'all')}
				>
					<Select.Trigger class="h-9 min-w-[140px]" data-testid="machines-role-filter">
						{roleFilter === 'engines' ? 'Model servers' : 'All roles'}
					</Select.Trigger>
					<Select.Content>
						<Select.Item value="all" label="All roles" />
						<Select.Item value="engines" label="Model servers" />
					</Select.Content>
				</Select.Root>
			</div>
			<div class="ml-auto flex items-center gap-2">
				<Button
					variant="outline"
					size="sm"
					class="gap-1.5"
					onclick={() => (tokensOpen = true)}
					data-testid="machines-tokens-button"
				>
					<KeyRound class="size-4" />
					Tokens
				</Button>
				<DropdownMenu>
					<DropdownMenuTrigger data-testid="machines-enroll-button">
						{#snippet child({ props })}
							<Button {...props} variant="default" size="sm" class="gap-1.5">
								<Plus class="size-4" />
								Enroll
							</Button>
						{/snippet}
					</DropdownMenuTrigger>
					<DropdownMenuContent align="end" class="w-40">
						<DropdownMenuItem onclick={() => openEnroll('runner')} data-testid="machines-enroll-runner">
							Runner…
						</DropdownMenuItem>
						<DropdownMenuItem onclick={() => openEnroll('worker')} data-testid="machines-enroll-worker">
							Worker…
						</DropdownMenuItem>
					</DropdownMenuContent>
				</DropdownMenu>
			</div>
		</div>
	{:else if !loading}
		<div class="flex items-center justify-between gap-3">
			<p class="text-sm text-muted-foreground tabular-nums" data-testid="machines-count">
				{allRows.length} machine{allRows.length === 1 ? '' : 's'} · {onlineCount} online
			</p>
			{#if onenroll}
				<Button variant="outline" size="sm" class="gap-1.5" onclick={onenroll}>
					<Plus class="size-4" />
					Enroll here
				</Button>
			{/if}
		</div>
	{/if}

	<!-- ── Error ──────────────────────────────────────────────────────────────── -->
	{#if error}
		<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{/if}

	<!-- ── Flat machine list ──────────────────────────────────────────────────── -->
	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">Loading…</div>
	{:else if allRows.length === 0}
		<div
			class="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border py-12 text-center"
		>
			<Server class="size-10 text-muted-foreground/40" />
			<p class="text-sm text-muted-foreground">
				{group != null ? 'No machines enrolled in this pool yet.' : 'No machines enrolled yet.'}
			</p>
			{#if group == null}
				<Button variant="outline" size="sm" class="gap-1.5" onclick={() => openEnroll('runner')}>
					<Plus class="size-4" />
					Enroll your first machine
				</Button>
			{:else if onenroll}
				<Button variant="outline" size="sm" class="gap-1.5" onclick={onenroll}>
					<Plus class="size-4" />
					Enroll here
				</Button>
			{/if}
		</div>
	{:else if rows.length === 0}
		<p class="px-1 py-2 text-sm text-muted-foreground">No machines match the current filters.</p>
	{:else}
		<div class="divide-y divide-border rounded-lg border border-border bg-card">
			{#each rows as row (row.kind + ':' + row.id)}
				{@const isModel = row.kind === 'runner' && modelServingIds.has(row.id)}
				<div
					class="group flex items-center justify-between p-4 transition-colors hover:bg-accent/40"
					data-testid="{row.kind}-item-{row.id}"
				>
					<div class="flex min-w-0 flex-1 items-start gap-3">
						<!-- Online dot -->
						<Tooltip.Provider>
							<Tooltip.Root>
								<Tooltip.Trigger>
									<StatusDot tone={row.online ? 'live' : 'idle'} class="mt-1" />
								</Tooltip.Trigger>
								<Tooltip.Content>
									{#if row.online}
										Online · last seen {row.lastSeen}
									{:else}
										Offline · last seen {row.lastSeen}
									{/if}
								</Tooltip.Content>
							</Tooltip.Root>
						</Tooltip.Provider>

						<!-- Identity -->
						<div class="min-w-0 flex-1">
							<div class="flex flex-wrap items-center gap-2">
								<span class="text-sm font-medium text-foreground">{row.name}</span>
								<Badge variant="outline" class="text-sm">{row.kind}</Badge>
								{#if isModel}
									<Badge variant="secondary" class="text-sm">model server</Badge>
								{/if}
								{#if row.kind === 'runner' && isStale(row.snap)}
									<Badge
										variant="outline"
										class="border-amber-500/50 text-xs text-amber-700 dark:text-amber-400"
										title="Online but reporting no host — restart this runner on a current build to de-anonymise it."
									>
										stale build
									</Badge>
								{/if}

								<!-- Pool membership — linked chip into the pool detail; hidden
									 when the table is already scoped to one pool. -->
								{#if group == null}
									{#if row.group}
										{@const capId = capacityIdByPath[row.group]}
										{#if capId}
											<a
												href="/fleet/{capId}"
												class="inline-flex items-center rounded-md bg-secondary px-2 py-0.5 font-mono text-xs text-secondary-foreground transition-colors hover:bg-secondary/70"
												data-testid="machine-pool-chip-{row.id}"
											>
												{row.group}
											</a>
										{:else}
											<span class="font-mono text-xs text-muted-foreground">{row.group}</span>
											<Badge
												variant="outline"
												class="border-amber-500/50 text-xs text-amber-700 dark:text-amber-400"
												title="This group is backed by no pool — the machine dispatches nothing."
											>
												no pool
											</Badge>
										{/if}
									{:else}
										<span class="text-xs text-muted-foreground/70">ungrouped</span>
									{/if}
								{/if}
							</div>

							<!-- Host fingerprint one-liner (runners only: accelerator ·
								 hostname · IP); falls back to the short id when the runner
								 reports no host. Workers show their freshness line instead. -->
							{#if row.kind === 'runner'}
								{#if hostSummary(row.snap?.host)}
									<p class="mt-1 truncate font-mono text-sm text-muted-foreground">
										{hostSummary(row.snap?.host)}
									</p>
								{:else}
									<p class="mt-1 truncate font-mono text-sm text-muted-foreground/70">
										{row.id}
									</p>
								{/if}
							{:else}
								<p class="mt-1 truncate text-sm text-muted-foreground">
									{row.backends.length} backend{row.backends.length === 1 ? '' : 's'} · {row.lastSeen}
								</p>
							{/if}

							<!-- Backends, collapsed: first 4 chips + a muted "+N" (the full
								 list lives in the detail drawer). -->
							{#if row.backends.length > 0}
								<div class="mt-1.5 flex flex-wrap items-center gap-1.5">
									<BackendChips backends={row.backends.slice(0, 4)} />
									{#if row.backends.length > 4}
										<span class="text-sm text-muted-foreground/70">
											+{row.backends.length - 4}
										</span>
									{/if}
								</div>
							{/if}

							<!-- Model-serving facet (Engines lens). Resident = loaded now; if
								 none resident, surface what's provisioned + ready to load. -->
							{#if row.kind === 'runner' && residentModels(row.id).length > 0}
								<p class="mt-1 flex items-center gap-1.5 truncate text-sm text-muted-foreground">
									<Cpu class="size-3.5 shrink-0" />
									<span class="font-mono">{residentModels(row.id).join(' · ')}</span>
								</p>
							{:else if row.kind === 'runner' && readyModels(row.id).length > 0}
								<p
									class="mt-1 flex items-center gap-1.5 truncate text-sm text-muted-foreground/80"
								>
									<Cpu class="size-3.5 shrink-0" />
									<span class="font-mono">{readyModels(row.id).join(' · ')}</span>
									<span class="shrink-0 text-muted-foreground/60">· ready to load</span>
								</p>
							{/if}
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
							onclick={() => {
								if (row.kind === 'runner') void openDetail(row.id);
								else workerDetail = row.worker ?? null;
							}}
							title="Machine details"
						>
							<Info class="size-3.5" />
							Details
						</Button>
						{#if row.kind === 'runner' && row.runner}
							{@const r = row.runner}
							<Button
								variant="ghost"
								size="sm"
								class="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
								onclick={() => handleRevoke(r)}
								disabled={revoking === row.id}
								title="Revoke runner"
							>
								<Trash2 class="size-3.5" />
								{revoking === row.id ? 'Revoking…' : 'Revoke'}
							</Button>
						{/if}
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>

<!-- ── Enroll sheet (mint + reveal-once) — full-inventory mode only; scoped mode
	 defers to the page band's single "Enroll here" via `onenroll`. ─────────── -->
{#if group == null}
	<EnrollSheet bind:open={enrollOpen} mode={enrollMode} group={null} onenrolled={() => void load(true)} />
	<TokensSheet bind:open={tokensOpen} />
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
					<dd class="col-span-2">
						<BackendChips
							backends={snap?.present ? (snap.backends ?? []) : []}
							empty={snap?.present ? '—' : 'offline (advertised on connect)'}
						/>
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

<!-- ── Worker detail drawer (compact — workers carry no host probe) ─────────── -->
<Sheet.Root
	open={workerDetail !== null}
	onOpenChange={(o: boolean) => {
		if (!o) workerDetail = null;
	}}
>
	<SheetContent class="w-[520px] overflow-y-auto sm:max-w-[520px]">
		<div class="space-y-4 p-2">
			<div class="space-y-1">
				<SheetTitle class="flex items-center gap-2 text-lg font-semibold">
					<Cpu class="size-4" />
					{workerDetail?.name ?? 'Worker'}
				</SheetTitle>
				<SheetDescription class="font-mono text-sm">{workerDetail?.id}</SheetDescription>
			</div>

			{#if workerDetail}
				<dl class="grid grid-cols-3 gap-x-3 gap-y-2 text-sm">
					<dt class="text-muted-foreground">Status</dt>
					<dd class="col-span-2">
						<Badge variant="outline" class="text-sm">{workerDetail.status}</Badge>
					</dd>

					<dt class="text-muted-foreground">Group</dt>
					<dd class="col-span-2">
						{#if workerDetail.group}<Badge variant="secondary" class="text-sm"
								>{workerDetail.group}</Badge
							>{:else}—{/if}
					</dd>

					<dt class="text-muted-foreground">Backends</dt>
					<dd class="col-span-2">
						<BackendChips backends={workerBackends(workerDetail)} empty="—" />
					</dd>

					<dt class="text-muted-foreground">Last seen</dt>
					<dd class="col-span-2">{fmtDate(workerDetail.last_seen_at)}</dd>

					<dt class="text-muted-foreground">Enrolled</dt>
					<dd class="col-span-2">{fmtDate(workerDetail.enrolled_at)}</dd>
				</dl>
			{/if}

			<SheetClose>
				<Button variant="outline" class="w-full">Close</Button>
			</SheetClose>
		</div>
	</SheetContent>
</Sheet.Root>
