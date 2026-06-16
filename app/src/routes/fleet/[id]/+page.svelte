<script lang="ts">
	// Uniform pool detail — ONE shell for every pool kind. The capacity is
	// resolved by id (the /fleet Pools-row deep-link `/fleet/{id}`), classified
	// into its operator-facing kind via the shared taxonomy in `pool-kinds.ts`,
	// and rendered as band (identity + kind chip + live line + per-kind enroll
	// action) over [Members | Interfaces (machine only) | Settings] tabs:
	//
	//   machine → MachinesTable scoped to this group (runners) + Interfaces.
	//   worker  → MachinesTable scoped to this group (pull workers).
	//   human   → PoolMembersHumans (roster + live presence + admin enroll).
	//   limit   → the seeded/in-use gauge + the live grant holders.
	//   cluster → managed on /clusters/{id}; this page just links out.
	//
	// Settings (all kinds): pool-net link (where a backing net exists), edit via
	// NewCapacityModal, delete, and — machine pools — the pool-scoped
	// registration tokens.
	import { tick } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import * as Tabs from '$lib/components/ui/tabs';
	import { PageShell, PageHeader } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { toast } from 'svelte-sonner';
	import UserPlus from '@lucide/svelte/icons/user-plus';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import Globe from '@lucide/svelte/icons/globe';
	import ArrowUpRight from '@lucide/svelte/icons/arrow-up-right';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import MachinesTable from '$lib/components/fleet/MachinesTable.svelte';
	import PoolMembersHumans from '$lib/components/fleet/PoolMembersHumans.svelte';
	import InterfacesCatalog from '$lib/components/fleet/InterfacesCatalog.svelte';
	import EnrollSheet from '$lib/components/fleet/EnrollSheet.svelte';
	import NewCapacityModal from '$lib/components/fleet/NewCapacityModal.svelte';
	import { poolKindOf, poolLiveLine, type PoolKind } from '$lib/components/fleet/pool-kinds';
	import { fmtDate } from '$lib/components/fleet/format';
	import { listCapacities, type CapacitySummary } from '$lib/api/capacities';
	import { isPlatformCapacity, canMutateCapacity } from '$lib/api/resource-tier';
	import { deleteResource } from '$lib/api/resources';
	import {
		listRegistrationTokens,
		revokeRegistrationToken,
		type RegistrationTokenSummary
	} from '$lib/api/runners';
	import { auth } from '$lib/auth/store.svelte';

	const resourceId = $derived(page.params.id ?? '');

	let capacity = $state<CapacitySummary | null>(null);
	let error = $state<string | null>(null);
	let activeTab = $state<'members' | 'interfaces' | 'settings'>('members');

	// Machine-pool registration tokens (Settings tab), scoped to this group.
	let tokens = $state<RegistrationTokenSummary[]>([]);
	let revokingToken = $state<string | null>(null);

	// Enroll sheet (runner / worker token mint, fixed to this pool's path).
	let enrollOpen = $state(false);
	// Edit via the same kind-switcher modal the list page uses (edit mode).
	let editOpen = $state(false);
	let deleting = $state(false);

	// Human pools: the band's "Enroll member" forwards into the roster
	// component's exported openEnroll() (it owns the HumanEnrollSheet).
	let humansRef = $state<{ openEnroll: () => void } | undefined>();

	const kind = $derived<PoolKind | null>(capacity ? poolKindOf(capacity) : null);
	const path = $derived(capacity?.path ?? null);
	const name = $derived(capacity?.display_name || capacity?.path || resourceId);
	const liveLine = $derived(capacity ? poolLiveLine(capacity) : null);
	const isAdmin = $derived(auth.isWorkspaceAdmin);

	// Platform-scoped pool (the shared worker `default` + `model_serving` pools):
	// badge + read-only note, and curation (edit / delete) requires platform admin.
	// `canMutateCapacity` folds the platform/tenant tiers into the per-row
	// `my_effective_role !== 'viewer'` signal the backend stamps.
	const isPlatform = $derived(capacity ? isPlatformCapacity(capacity) : false);
	const canCurate = $derived(capacity ? canMutateCapacity(capacity) : false);

	/** Members-tab label per kind: Members / Workers / Holders. */
	const membersLabel = $derived(
		kind?.id === 'worker' ? 'Workers' : kind?.id === 'limit' ? 'Holders' : 'Members'
	);
	/** Kinds whose capacity is backed by a deployed pool net (deep-linkable). */
	const hasPoolNet = $derived(
		kind?.id === 'machine' || kind?.id === 'human' || kind?.id === 'limit'
	);

	async function load() {
		try {
			const all = await listCapacities();
			const cap = all.find((c) => c.id === resourceId) ?? null;
			capacity = cap;
			error = cap ? null : 'Pool not found.';
			if (cap && poolKindOf(cap).id === 'machine') {
				// Pool-scoped runner registration tokens; fail-soft — the token list
				// is Settings-tab garnish, never let it wipe the page.
				try {
					const tPage = await listRegistrationTokens({ perPage: 200 });
					tokens = tPage.items.filter((t) => t.group === cap.path);
				} catch {
					tokens = [];
				}
			} else {
				tokens = [];
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load pool';
		}
	}

	$effect(() => {
		void resourceId;
		activeTab = 'members';
		void load();
		const t = setInterval(() => void load(), 5000);
		return () => clearInterval(t);
	});

	// ── Band actions ────────────────────────────────────────────────────────────

	/** Machine / worker pools: open the token-mint sheet fixed to this pool. */
	function openEnroll() {
		enrollOpen = true;
	}

	/** Human pools: jump to the roster (it owns the enroll sheet) and open it. */
	async function openEnrollMember() {
		activeTab = 'members';
		await tick();
		humansRef?.openEnroll();
	}

	async function handleDelete() {
		if (deleting) return;
		if (!confirm(`Delete pool “${name}”? Its backing net (if any) is retired.`)) return;
		deleting = true;
		try {
			await deleteResource(resourceId);
			toast.success('Pool deleted.');
			await goto('/fleet');
		} catch (e) {
			toast.error(`Delete failed: ${e instanceof Error ? e.message : e}`);
		} finally {
			deleting = false;
		}
	}

	async function handleRevokeToken(token: RegistrationTokenSummary) {
		if (revokingToken) return;
		if (
			!confirm(
				"Revoke this registration token? Runners that haven't enrolled yet won't be able to use it."
			)
		)
			return;
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
</script>

<PageShell width="wide" testid="pool-detail-page">
	{#snippet band()}
		<PageHeader
			title={name}
			variant="detail"
			back={{ href: '/fleet', label: 'Fleet' }}
			headTitle={`${name} | Fleet | Mekhan`}
			titleTestid="pool-detail-title"
		>
			{#snippet children()}
				<div class="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
					{#if kind}
						<Badge variant="secondary">{kind.chip}</Badge>
						<span class="text-xs text-muted-foreground">{kind.plainAxes}</span>
					{/if}
					{#if isPlatform}
						<Badge class="gap-1 bg-sky-100 text-sky-800" data-testid="pool-detail-platform-badge">
							<Globe class="size-3" /> Platform (shared)
						</Badge>
					{/if}
					{#if capacity}
						<span class="font-mono text-xs text-muted-foreground">{capacity.path}</span>
					{:else}
						<span class="font-mono text-xs text-muted-foreground">{resourceId}</span>
					{/if}
					{#if liveLine}
						<span class="text-xs text-muted-foreground tabular-nums">{liveLine}</span>
					{/if}
				</div>
				{#if isPlatform}
					<p
						class="mt-1.5 flex items-start gap-1.5 text-xs text-muted-foreground"
						data-testid="pool-detail-platform-note"
					>
						<Globe class="mt-0.5 size-3.5 shrink-0 text-sky-600" />
						<span>
							Shared platform pool — managed by platform admins, runnable by every workspace.{#if !canCurate}
								{' '}You have read-only access.{/if}
						</span>
					</p>
				{/if}
			{/snippet}
			{#snippet actions()}
				<!-- Enrolling compute INTO a platform pool is platform-admin curation
				     (distinct from RUNNING workloads on it, which any workspace may do),
				     so the affordance is hidden for non-admins on platform pools. -->
				{#if kind?.id === 'machine' && (!isPlatform || canCurate)}
					<Button
						variant="default"
						size="sm"
						class="gap-1.5"
						onclick={openEnroll}
						data-testid="pool-detail-enroll"
					>
						<UserPlus class="size-4" />
						Enroll runner
					</Button>
				{:else if kind?.id === 'worker' && (!isPlatform || canCurate)}
					<Button
						variant="default"
						size="sm"
						class="gap-1.5"
						onclick={openEnroll}
						data-testid="pool-detail-enroll"
					>
						<UserPlus class="size-4" />
						Enroll worker
					</Button>
				{:else if kind?.id === 'human' && isAdmin}
					<Button
						variant="default"
						size="sm"
						class="gap-1.5"
						onclick={() => void openEnrollMember()}
						data-testid="pool-detail-enroll"
					>
						<UserPlus class="size-4" />
						Enroll member
					</Button>
				{/if}
			{/snippet}
		</PageHeader>
	{/snippet}
	{#snippet tabs()}
		<Tabs.Root
			value={activeTab}
			onValueChange={(v) => (activeTab = (v as typeof activeTab) ?? 'members')}
		>
			<Tabs.List variant="underline">
				<Tabs.Trigger
					variant="underline"
					value="members"
					data-testid={kind?.id === 'machine' ? 'group-tab-runners' : 'pool-tab-members'}
				>
					{membersLabel}
				</Tabs.Trigger>
				{#if kind?.id === 'machine'}
					<Tabs.Trigger variant="underline" value="interfaces" data-testid="group-tab-interfaces">
						Interfaces
					</Tabs.Trigger>
				{/if}
				<Tabs.Trigger variant="underline" value="settings" data-testid="pool-tab-settings">
					Settings
				</Tabs.Trigger>
			</Tabs.List>
		</Tabs.Root>
	{/snippet}

	{#if error}
		<div
			class="mb-3 rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive"
		>
			{error}
		</div>
	{/if}

	{#if capacity && kind}
		{#if activeTab === 'members'}
			{#if kind.id === 'machine' || kind.id === 'worker'}
				<!-- MACHINE / WORKER — the flat machines inventory scoped to this pool
					 (runners.group / workers.group === path). The band owns the enroll
					 action; onenroll forwards "Enroll here" back to it. -->
				<MachinesTable group={path} platform={isPlatform} onenroll={openEnroll} />
			{:else if kind.id === 'human'}
				<!-- HUMAN — roster members + live presence; owns its enroll sheet. -->
				<PoolMembersHumans
					bind:this={humansRef}
					capacityId={resourceId}
					capacityName={capacity.display_name}
				/>
			{:else if kind.id === 'limit'}
				<!-- LIMIT — the seeded/in-use gauge + the live grant holders. -->
				{#if capacity.live.kind === 'tokens'}
					{@const live = capacity.live}
					<div class="space-y-4" data-testid="group-holders">
						<div class="flex items-center gap-3 rounded-lg border border-border bg-card px-4 py-3">
							<KeyRound class="size-5 text-muted-foreground" />
							<div>
								<p class="text-sm font-medium text-foreground tabular-nums">
									{live.in_use}/{live.seeded} in use
								</p>
								<p class="text-xs text-muted-foreground">
									{live.seeded - live.in_use} token{live.seeded - live.in_use === 1 ? '' : 's'} free
								</p>
							</div>
						</div>

						<div class="space-y-2">
							<h4 class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
								Holders
							</h4>
							{#if live.holders.length === 0}
								<div
									class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-10"
								>
									<KeyRound class="size-10 text-muted-foreground/40" />
									<p class="mt-2 text-sm text-muted-foreground">No tokens held.</p>
								</div>
							{:else}
								<div class="overflow-hidden rounded-lg border border-border">
									<table class="w-full text-sm">
										<thead class="bg-muted/50 text-muted-foreground">
											<tr>
												<th class="px-3 py-2 text-left font-medium">Instance</th>
												<th class="px-3 py-2 text-left font-medium">Since</th>
											</tr>
										</thead>
										<tbody>
											{#each live.holders as h, i (h.instance_id ?? i)}
												<tr class="border-t border-border">
													<td class="px-3 py-2 font-mono text-xs text-foreground">
														{h.instance_id ?? '—'}
													</td>
													<td class="px-3 py-2 text-muted-foreground">{fmtDate(h.since)}</td>
												</tr>
											{/each}
										</tbody>
									</table>
								</div>
							{/if}
						</div>
					</div>
				{/if}
			{:else if kind.id === 'cluster'}
				<!-- CLUSTER — detailed on the cluster page, not here. -->
				<div class="rounded-lg border border-border bg-card px-4 py-3 text-sm">
					This pool is an external scheduler cluster — it is managed on the cluster page.
					<a class="font-medium text-foreground underline" href="/clusters/{resourceId}">
						Open cluster →
					</a>
				</div>
			{:else}
				<!-- BROKEN — axes failed to parse (fail-closed); fix it in Settings. -->
				<div class="rounded-lg border border-border bg-card px-4 py-3 text-sm text-muted-foreground">
					{kind.plainAxes}.
				</div>
			{/if}
		{:else if activeTab === 'interfaces' && kind.id === 'machine' && path}
			<InterfacesCatalog group={path} />
		{:else if activeTab === 'settings'}
			<div class="space-y-6" data-testid="pool-settings">
				<!-- Pool record: edit via the kind-switcher modal + the backing-net link. -->
				<section class="space-y-2">
					<h4 class="text-sm font-medium uppercase tracking-wide text-muted-foreground">Pool</h4>
					<div
						class="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border bg-card px-4 py-3"
					>
						<div class="min-w-0">
							<p class="text-sm font-medium text-foreground">{name}</p>
							<p class="truncate font-mono text-xs text-muted-foreground">{capacity.path}</p>
						</div>
						<div class="flex shrink-0 items-center gap-2">
							{#if hasPoolNet}
								<Button
									href="/nets/pool-{resourceId}"
									variant="outline"
									size="sm"
									class="gap-1.5"
									data-testid="view-pool-net"
								>
									<ArrowUpRight class="size-4" />
									View pool net
								</Button>
							{/if}
							{#if canCurate}
								<Button
									variant="outline"
									size="sm"
									class="gap-1.5"
									onclick={() => (editOpen = true)}
									data-testid="pool-edit-button"
								>
									<Pencil class="size-3.5" />
									Edit
								</Button>
							{/if}
						</div>
					</div>
				</section>

				{#if kind.id === 'machine'}
					<!-- Pool-scoped registration tokens (runner enrollment). -->
					<section class="space-y-2">
						<h4 class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
							Registration tokens
						</h4>
						{#if tokens.length === 0}
							<p class="rounded-lg border border-dashed border-border px-4 py-3 text-sm text-muted-foreground">
								No registration tokens for this pool. “Enroll runner” mints one.
							</p>
						{:else}
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
						{/if}
					</section>
				{/if}

				<!-- Danger zone — curation only (platform admin for platform pools,
					 workspace role for tenant pools). Read-only viewers don't see it. -->
				{#if canCurate}
					<section class="space-y-2">
						<h4 class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
							Danger zone
						</h4>
						<div
							class="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-destructive/40 bg-destructive/5 px-4 py-3"
						>
							<p class="text-sm text-muted-foreground">
								Delete this pool. Its backing net (if any) is retired.
							</p>
							<Button
								variant="outline"
								size="sm"
								class="gap-1.5 text-destructive hover:bg-destructive/10 hover:text-destructive"
								onclick={handleDelete}
								disabled={deleting}
								data-testid="pool-delete-button"
							>
								<Trash2 class="size-3.5" />
								{deleting ? 'Deleting…' : 'Delete pool'}
							</Button>
						</div>
					</section>
				{/if}
			</div>
		{/if}
	{/if}
</PageShell>

<!-- Enroll flow (machine / worker pools): mint + reveal-once, fixed to this
	 pool's path. Human pools enroll via PoolMembersHumans' own sheet. -->
<EnrollSheet
	bind:open={enrollOpen}
	mode={kind?.id === 'worker' ? 'worker' : 'runner'}
	group={path}
	groupIsPlatform={isPlatform}
	onenrolled={() => void load()}
/>

<!-- Edit: the same kind-switcher modal as the list page, in edit mode
	 (kind + name locked, fields prefilled from the summary). -->
<NewCapacityModal
	bind:open={editOpen}
	editing={capacity}
	onsaved={() => {
		editOpen = false;
		void load();
	}}
/>
