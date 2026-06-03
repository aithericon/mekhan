<script lang="ts">
	// Fleet → Runner list, SPLIT INTO ITS GROUPS.
	// On mount: load listRunners() + getRunnerPresence() + the runner_group
	// resources, then groupFleet() into sections (one per backed group, then any
	// unbacked aliases, then ungrouped). A runner's `group` is only meaningful
	// when backed by a `runner_group` resource — that resource carries the
	// presence-pool net the runner's unit is admitted into — so the enroll dialog
	// only lets you pick an EXISTING group (or create one inline first).
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription,
		SheetClose
	} from '$lib/components/ui/sheet';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { toast } from 'svelte-sonner';
	import Server from '@lucide/svelte/icons/server';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import Terminal from '@lucide/svelte/icons/terminal';
	import Info from '@lucide/svelte/icons/info';
	import RadioTower from '@lucide/svelte/icons/radio-tower';
	import {
		listRunners,
		getRunner,
		getRunnerPresence,
		revokeRunner,
		listRegistrationTokens,
		createRegistrationToken,
		revokeRegistrationToken,
		type RunnerSummary,
		type RunnerDetail,
		type RunnerPresenceSnapshot,
		type RegistrationTokenSummary,
		type CreatedRegistrationToken
	} from '$lib/api/runners';
	import { listResources, createResource, type ResourceSummary } from '$lib/api/resources';
	import { groupFleet, type FleetSection } from './grouping';

	// ── State ──────────────────────────────────────────────────────────────────

	let runners = $state<RunnerSummary[]>([]);
	let presence = $state<RunnerPresenceSnapshot[]>([]);
	let groups = $state<ResourceSummary[]>([]);
	let tokens = $state<RegistrationTokenSummary[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Filters
	let onlineFilter = $state<'all' | 'online'>('all');

	// Revoke
	let revoking = $state<string | null>(null);
	let revokingToken = $state<string | null>(null);
	let backingGroup = $state<string | null>(null);

	// Enroll modal. `enrollGroupSel` is one of: '' (no group), an existing group
	// alias, or the '__new__' sentinel (create a new group inline before minting).
	const NEW_GROUP = '__new__';
	let enrollOpen = $state(false);
	let enrollName = $state('');
	let enrollGroupSel = $state('');
	let enrollNewGroup = $state('');
	let enrollMaxUses = $state('');
	let enrollReusable = $state(false);
	let enrollExpiresAt = $state('');
	let enrolling = $state(false);

	// Reveal-once token sheet
	let revealed = $state<(CreatedRegistrationToken & { name: string; group: string }) | null>(null);

	// Detail drawer (full record incl. capabilities + nats_public_key)
	let detail = $state<RunnerDetail | null>(null);
	let detailLoading = $state(false);
	let detailError = $state<string | null>(null);

	// ── Derived ────────────────────────────────────────────────────────────────

	/** Fast lookup: runner_id → presence snapshot */
	const presenceById = $derived(Object.fromEntries(presence.map((p) => [p.runner_id, p])));

	/** The fleet split into ordered sections (backed → unbacked → ungrouped). */
	const sections = $derived(groupFleet(runners, presenceById, groups));

	/** Apply the online-only filter to a section's runners for display. */
	function shown(section: FleetSection): RunnerSummary[] {
		if (onlineFilter !== 'online') return section.runners;
		return section.runners.filter((r) => presenceById[r.id]?.present);
	}

	// ── Load ───────────────────────────────────────────────────────────────────

	async function load() {
		loading = true;
		error = null;
		try {
			const [rPage, pSnaps, gPage, tPage] = await Promise.all([
				listRunners({ perPage: 200 }),
				getRunnerPresence(),
				listResources({ resource_type: 'runner_group', perPage: 200 }),
				listRegistrationTokens({ perPage: 200 })
			]);
			runners = rPage.items;
			presence = pSnaps;
			groups = gPage.items;
			tokens = tPage.items;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load runners';
			runners = [];
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		load();
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

	/** Back an UNBACKED group: create its `runner_group` resource (deploys the
	    pool net), upgrading present-but-unadmitted runners on the next heartbeat. */
	async function handleBackGroup(alias: string) {
		if (backingGroup) return;
		backingGroup = alias;
		try {
			await createResource({ path: alias, resource_type: 'runner_group', config: {} });
			toast.success(`Group "${alias}" created — pool net deployed.`);
			await load();
		} catch (e) {
			toast.error(`Could not create group: ${e instanceof Error ? e.message : e}`);
		} finally {
			backingGroup = null;
		}
	}

	function openEnroll() {
		enrollName = '';
		enrollGroupSel = '';
		enrollNewGroup = '';
		enrollMaxUses = '';
		enrollReusable = false;
		enrollExpiresAt = '';
		enrollOpen = true;
	}

	async function handleEnroll(e: Event) {
		e.preventDefault();
		if (enrolling) return;
		enrolling = true;
		try {
			// Resolve the chosen group. Creating a new one deploys its pool net
			// BEFORE the token is minted, so the backend's backed-group check passes
			// and the enrolled runner is guaranteed a pool to be admitted into.
			let group: string | undefined;
			if (enrollGroupSel === NEW_GROUP) {
				const path = enrollNewGroup.trim();
				if (!path) {
					toast.error('Enter a name for the new group.');
					enrolling = false;
					return;
				}
				try {
					await createResource({ path, resource_type: 'runner_group', config: {} });
				} catch (ce) {
					// A 409 (already exists) is fine — reuse it; anything else aborts.
					const msg = ce instanceof Error ? ce.message : String(ce);
					if (!msg.includes('409')) throw ce;
				}
				group = path;
			} else if (enrollGroupSel) {
				group = enrollGroupSel;
			}

			const created = await createRegistrationToken({
				group,
				// Always send the explicit checkbox value: the backend defaults an
				// OMITTED `reusable` to `true`, so `enrollReusable || undefined` would
				// silently mint a reusable token whenever the box is left unchecked.
				reusable: enrollReusable,
				max_uses: enrollMaxUses ? parseInt(enrollMaxUses, 10) : undefined,
				expires_at: enrollExpiresAt ? `${enrollExpiresAt}T23:59:59Z` : undefined
			});
			// Stash the name/group alongside the revealed token so the CLI snippet can use them.
			revealed = { ...created, name: enrollName.trim(), group: group ?? '' };
			enrollOpen = false;
			toast.success('Token minted — copy it now.');
			await load();
		} catch (e) {
			toast.error(`Enroll failed: ${e instanceof Error ? e.message : e}`);
		} finally {
			enrolling = false;
		}
	}

	// ── Formatting helpers ─────────────────────────────────────────────────────

	function fmtDate(ts: string | null | undefined): string {
		if (!ts) return '—';
		const d = new Date(ts);
		return Number.isNaN(d.getTime()) ? ts : d.toLocaleString();
	}

	function fmtMsAgo(ms: number): string {
		if (ms < 2000) return 'just now';
		if (ms < 60_000) return `${Math.round(ms / 1000)}s ago`;
		if (ms < 3_600_000) return `${Math.round(ms / 60_000)}m ago`;
		return `${Math.round(ms / 3_600_000)}h ago`;
	}

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

	/** Build the CLI enroll line shown in the reveal sheet. */
	function cliLine(token: string, name: string, group: string): string {
		const origin = typeof window !== 'undefined' ? window.location.origin : '';
		let cmd = `aithericon-executor register --url ${origin} --token ${token}`;
		if (name) cmd += ` --name ${name}`;
		if (group) cmd += ` --group ${group}`;
		return cmd;
	}
</script>

<!-- ── Toolbar ──────────────────────────────────────────────────────────────── -->
<div class="space-y-4" data-testid="runner-list">
	<div class="flex flex-wrap items-center gap-3">
		<!-- Online/all filter -->
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

	<!-- ── Error ──────────────────────────────────────────────────────────────── -->
	{#if error}
		<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{/if}

	<!-- ── Sectioned runner list (one block per group) ──────────────────────────── -->
	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
			Loading…
		</div>
	{:else if runners.length === 0 && groups.length === 0}
		<div
			class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16"
		>
			<Server class="size-10 text-muted-foreground/40" />
			<p class="mt-3 text-sm text-muted-foreground">No runners enrolled yet.</p>
			<Button variant="outline" size="sm" class="mt-4 gap-1.5" onclick={openEnroll}>
				<Plus class="size-4" />
				Enroll your first runner
			</Button>
		</div>
	{:else}
		<div class="space-y-6">
			{#each sections as section (section.kind + ':' + (section.alias ?? '∅'))}
				{@const rows = shown(section)}
				<section data-testid="group-section-{section.alias ?? 'ungrouped'}">
					<!-- Section header -->
					{#if section.kind === 'unbacked'}
						<div
							class="mb-2 flex flex-wrap items-center gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2"
						>
							<TriangleAlert class="size-4 shrink-0 text-amber-600" />
							<span class="font-medium text-amber-800 dark:text-amber-400">{section.alias}</span>
							<Badge variant="outline" class="border-amber-500/50 text-amber-700 dark:text-amber-400"
								>no pool · unbacked</Badge
							>
							<span class="text-xs text-amber-700/90 dark:text-amber-400/80">
								These runners heartbeat but are admitted to no pool — no
								<code class="font-mono">runner_group</code> resource backs this alias.
							</span>
							<Button
								variant="outline"
								size="sm"
								class="ml-auto h-7 gap-1 border-amber-500/50 text-amber-800 hover:bg-amber-500/20 dark:text-amber-300"
								onclick={() => section.alias && handleBackGroup(section.alias)}
								disabled={backingGroup === section.alias}
							>
								<Plus class="size-3.5" />
								{backingGroup === section.alias ? 'Creating…' : 'Create group'}
							</Button>
						</div>
					{:else if section.kind === 'ungrouped'}
						<div class="mb-2 flex items-center gap-2 border-b border-border pb-1.5">
							<span class="text-sm font-semibold text-muted-foreground">Ungrouped</span>
							<span class="text-xs text-muted-foreground">· not assigned to a group</span>
						</div>
					{:else}
						<div class="mb-2 flex flex-wrap items-center gap-2 border-b border-border pb-1.5">
							<RadioTower class="size-4 shrink-0 text-muted-foreground" />
							<span class="text-sm font-semibold text-foreground">{section.alias}</span>
							<Badge variant="outline" class="text-xs">pool ready</Badge>
							<span class="text-xs text-muted-foreground tabular-nums">
								{section.onlineCount}/{section.runners.length} online
							</span>
							{#if section.backends.length > 0}
								<span class="ml-1 text-xs text-muted-foreground">covers</span>
								<div class="flex flex-wrap gap-1">
									{#each section.backends as be (be)}
										<Badge
											variant="outline"
											class="px-1.5 py-0 text-[10px] font-normal text-muted-foreground">{be}</Badge
										>
									{/each}
								</div>
							{/if}
						</div>
					{/if}

					<!-- Section rows -->
					{#if rows.length === 0}
						<p class="px-1 py-2 text-xs text-muted-foreground">
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
								<div
									class="group flex items-center justify-between rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent/40"
									data-testid="runner-item-{runner.id}"
								>
									<div class="flex min-w-0 flex-1 items-start gap-3">
										<!-- Online dot -->
										<Tooltip.Provider>
											<Tooltip.Root>
												<Tooltip.Trigger>
													<span
														class="mt-1 inline-block size-2.5 shrink-0 rounded-full {online
															? 'bg-emerald-500'
															: 'bg-muted-foreground/30'}"
													></span>
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
												<span class="font-medium text-foreground">{runner.name}</span>
												<Badge variant="outline">{runner.status}</Badge>
											</div>
											<p class="mt-1 truncate font-mono text-xs text-muted-foreground">
												{runner.id}
											</p>
											<p class="mt-0.5 truncate text-xs text-muted-foreground">
												Caps: <span class="font-mono"
													>{capsSummary(runner.capabilities as Record<string, unknown>)}</span
												>
											</p>
											<p class="mt-0.5 truncate text-xs text-muted-foreground">
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
									<Badge variant="secondary">{token.group}</Badge>
								{/if}
								<Badge variant="outline">
									{token.reusable ? 'reusable' : `1-shot · ${token.uses} used`}
								</Badge>
								{#if token.max_uses}
									<span class="text-xs text-muted-foreground">max {token.max_uses}</span>
								{/if}
							</div>
							<p class="text-xs text-muted-foreground">
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

<!-- ── Enroll modal (mint registration token) ──────────────────────────────── -->
<Sheet.Root
	open={enrollOpen}
	onOpenChange={(o: boolean) => {
		if (!o) enrollOpen = false;
	}}
>
	<SheetContent class="w-[480px] sm:max-w-[480px]">
		<div class="space-y-4 p-2">
			<div class="space-y-1">
				<SheetTitle class="flex items-center gap-2 text-lg font-semibold">
					<Plus class="size-4" />
					Enroll a new runner
				</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					Mint a one-time registration token. Hand it to the executor — it enrolls itself using
					<code class="rounded bg-muted px-1 py-0.5 font-mono text-xs">
						aithericon-executor register
					</code>.
				</SheetDescription>
			</div>

			<form class="space-y-3" onsubmit={handleEnroll}>
				<div class="space-y-1">
					<label
						for="enroll-name"
						class="text-sm font-medium uppercase tracking-wide text-muted-foreground"
					>
						Runner name
					</label>
					<Input id="enroll-name" bind:value={enrollName} required placeholder="e.g. gpu-node-01" />
					<p class="text-xs text-muted-foreground">
						Required — the generated <code>register</code> command needs
						<code>--name</code>.
					</p>
				</div>

				<!-- Group: pick an existing runner_group, none, or create one inline. -->
				<div class="space-y-1">
					<label
						for="enroll-group"
						class="text-sm font-medium uppercase tracking-wide text-muted-foreground"
					>
						Group
					</label>
					<Select.Root
						type="single"
						value={enrollGroupSel}
						onValueChange={(v) => (enrollGroupSel = v ?? '')}
					>
						<Select.Trigger id="enroll-group" class="h-9 w-full" data-testid="enroll-group-select">
							{#if enrollGroupSel === NEW_GROUP}
								Create new group…
							{:else if enrollGroupSel}
								{enrollGroupSel}
							{:else}
								No group
							{/if}
						</Select.Trigger>
						<Select.Content>
							<Select.Item value="" label="No group" />
							{#each groups as g (g.id)}
								<Select.Item value={g.path} label={g.path} />
							{/each}
							<Select.Item value={NEW_GROUP} label="＋ Create new group…" />
						</Select.Content>
					</Select.Root>
					{#if enrollGroupSel === NEW_GROUP}
						<Input
							class="mt-1"
							bind:value={enrollNewGroup}
							placeholder="new group name, e.g. gpu_lab"
							data-testid="enroll-new-group"
						/>
						<p class="text-xs text-muted-foreground">
							Creates a <code class="font-mono">runner_group</code> resource (snake_case) and deploys
							its pool net before minting the token.
						</p>
					{:else}
						<p class="text-xs text-muted-foreground">
							A group must be backed by a <code class="font-mono">runner_group</code> resource — its
							pool net is where the runner's unit is admitted.
						</p>
					{/if}
				</div>

				<div class="grid gap-3 sm:grid-cols-2">
					<div class="space-y-1">
						<label
							for="enroll-max"
							class="text-sm font-medium uppercase tracking-wide text-muted-foreground"
						>
							Max uses <span class="normal-case">(optional)</span>
						</label>
						<Input
							id="enroll-max"
							type="number"
							min="1"
							bind:value={enrollMaxUses}
							placeholder="unlimited"
						/>
					</div>
					<div class="space-y-1">
						<label
							for="enroll-expires"
							class="text-sm font-medium uppercase tracking-wide text-muted-foreground"
						>
							Expires <span class="normal-case">(optional)</span>
						</label>
						<Input id="enroll-expires" type="date" bind:value={enrollExpiresAt} />
					</div>
				</div>

				<div class="flex items-center gap-2">
					<input
						id="enroll-reusable"
						type="checkbox"
						bind:checked={enrollReusable}
						class="size-4 rounded border-border"
					/>
					<label for="enroll-reusable" class="text-sm text-muted-foreground"> Reusable token </label>
				</div>

				<div class="flex gap-2 pt-1">
					<Button type="submit" disabled={enrolling || !enrollName.trim()} class="flex-1">
						{enrolling ? 'Minting…' : 'Mint token'}
					</Button>
					<SheetClose>
						<Button type="button" variant="outline">Cancel</Button>
					</SheetClose>
				</div>
			</form>
		</div>
	</SheetContent>
</Sheet.Root>

<!-- ── Reveal-once token sheet ─────────────────────────────────────────────── -->
<Sheet.Root
	open={revealed !== null}
	onOpenChange={(o: boolean) => {
		if (!o) revealed = null;
	}}
>
	<SheetContent class="w-[520px] sm:max-w-[520px]">
		<div class="space-y-4 p-2">
			<div class="space-y-1">
				<SheetTitle class="flex items-center gap-2 text-lg font-semibold">
					<Terminal class="size-4" />
					Registration token
				</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					Copy this now — it is not stored and will never be shown again.
				</SheetDescription>
			</div>

			<div
				class="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 p-2 text-sm text-amber-700 dark:text-amber-400"
			>
				<TriangleAlert class="mt-0.5 size-3.5 shrink-0" />
				<span>Anyone with this token can enroll an executor that acts on your behalf.</span>
			</div>

			{#if revealed}
				<!-- Token secret -->
				<div>
					<p class="mb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">Token</p>
					<div class="flex items-center gap-2">
						<code
							class="flex-1 break-all rounded bg-muted px-2 py-1.5 font-mono text-sm text-foreground"
							data-testid="token-secret"
						>
							{revealed.token}
						</code>
						<CopyButton text={revealed.token} />
					</div>
				</div>

				<!-- CLI enroll line -->
				<div>
					<p class="mb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
						Ready-to-paste CLI command
					</p>
					<div class="flex items-start gap-2">
						<code class="flex-1 break-all rounded bg-muted px-2 py-1.5 font-mono text-xs text-foreground">
							{cliLine(revealed.token, revealed.name, revealed.group)}
						</code>
						<CopyButton text={cliLine(revealed.token, revealed.name, revealed.group)} />
					</div>
				</div>
			{/if}

			<SheetClose>
				<Button variant="outline" class="w-full">Done</Button>
			</SheetClose>
		</div>
	</SheetContent>
</Sheet.Root>

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
				<SheetDescription class="font-mono text-xs">{detail?.id}</SheetDescription>
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
					<dd class="col-span-2"><Badge variant="outline">{detail.status}</Badge></dd>

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
						{#if detail.group}<Badge variant="secondary">{detail.group}</Badge>{:else}—{/if}
					</dd>

					<dt class="text-muted-foreground">Backends</dt>
					<dd class="col-span-2">
						{#if snap && (snap.backends?.length ?? 0) > 0}
							<div class="flex flex-wrap gap-1">
								{#each snap.backends ?? [] as be (be)}
									<Badge variant="outline" class="text-xs font-normal">{be}</Badge>
								{/each}
							</div>
						{:else}
							<span class="text-muted-foreground"
								>{snap?.present ? '—' : 'offline (advertised on connect)'}</span
							>
						{/if}
					</dd>

					<dt class="text-muted-foreground">Last seen</dt>
					<dd class="col-span-2">{fmtDate(detail.last_seen_at)}</dd>

					<dt class="text-muted-foreground">Enrolled</dt>
					<dd class="col-span-2">{fmtDate(detail.enrolled_at)}</dd>

					{#if detail.nats_public_key}
						<dt class="text-muted-foreground">NATS key</dt>
						<dd class="col-span-2 break-all font-mono text-xs">{detail.nats_public_key}</dd>
					{/if}
				</dl>

				<div class="space-y-1">
					<h3 class="text-sm font-medium text-muted-foreground">Capabilities</h3>
					<pre
						class="max-h-72 overflow-auto rounded-md border border-border bg-muted/40 p-3 font-mono text-xs">{capsPretty(
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
