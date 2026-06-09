<script lang="ts">
	// The Control-Plane view of the HUMAN capacity pools (docs/33 P4). The human
	// counterpart to the runner-presence cards: each `offer`-dispatch presence
	// capacity is a card showing its enrolled roster members and their LIVE
	// presence (admitted vs offline), polled alongside the rest of the board.
	//
	// "Live presence" here is the in-memory admission snapshot from
	// `GET /api/v1/human-presence` (which members mekhan currently considers in
	// their pool), joined to the durable `roster_members` enrollment list. A
	// member can be enrolled-but-offline (no presence row, or `present:false`).
	//
	// Workspace admins also get the enrollment controls here — enroll a member,
	// edit caps/concurrency/availability, or revoke — all gated on
	// `auth.isWorkspaceAdmin` (hidden for everyone else). They drive a single
	// shared HumanEnrollSheet instance.
	import type { CapacitySummary } from '$lib/api/capacities';
	import {
		getHumanPresence,
		listRoster,
		revokeMember,
		type HumanPresenceSnapshot,
		type RosterMemberSummary
	} from '$lib/api/roster';
	import { auth } from '$lib/auth/store.svelte';
	import StatusDot from '$lib/components/fleet/StatusDot.svelte';
	import HumanEnrollSheet from '$lib/components/fleet/HumanEnrollSheet.svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import Users from '@lucide/svelte/icons/users';
	import UserPlus from '@lucide/svelte/icons/user-plus';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { toast } from 'svelte-sonner';

	let { capacities }: { capacities: CapacitySummary[] } = $props();

	const isAdmin = $derived(auth.isWorkspaceAdmin);

	let presence = $state<HumanPresenceSnapshot[]>([]);
	let members = $state<RosterMemberSummary[]>([]);

	// ── Enroll/edit sheet state (single shared instance) ───────────────────────
	let sheetOpen = $state(false);
	let sheetCapacityId = $state('');
	let sheetCapacityName = $state('');
	let sheetMember = $state<RosterMemberSummary | null>(null);

	async function poll() {
		const [p, m] = await Promise.all([
			getHumanPresence().catch(() => [] as HumanPresenceSnapshot[]),
			listRoster().catch(() => [] as RosterMemberSummary[])
		]);
		presence = p;
		members = m;
	}

	$effect(() => {
		void poll();
		const t = setInterval(() => void poll(), 5000);
		return () => clearInterval(t);
	});

	function openEnroll(cap: CapacitySummary) {
		sheetMember = null;
		sheetCapacityId = cap.id;
		sheetCapacityName = cap.display_name;
		sheetOpen = true;
	}

	function openEdit(cap: CapacitySummary, m: RosterMemberSummary) {
		sheetMember = m;
		sheetCapacityId = cap.id;
		sheetCapacityName = cap.display_name;
		sheetOpen = true;
	}

	async function handleRevoke(m: RosterMemberSummary) {
		const who = m.member_display_name ?? m.member_email ?? short(m.member_user_id);
		if (!confirm(`Revoke ${who} from this pool? They will stop receiving offers.`)) return;
		try {
			await revokeMember(m.id);
			toast.success('Member revoked.');
			await poll();
		} catch (e) {
			toast.error(`Revoke failed: ${e instanceof Error ? e.message : e}`);
		}
	}

	function membersFor(capId: string): RosterMemberSummary[] {
		return members.filter((m) => m.capacity_id === capId);
	}

	/** Member user-ids already enrolled into a capacity — fed to the enroll picker
	 *  so it excludes them. */
	function memberIdsFor(capId: string): string[] {
		return membersFor(capId).map((m) => m.member_user_id);
	}

	function presentFor(capId: string, memberId: string): HumanPresenceSnapshot | undefined {
		return presence.find((p) => p.capacity_id === capId && p.member_user_id === memberId);
	}

	function onlineCount(capId: string): number {
		return presence.filter((p) => p.capacity_id === capId && p.present).length;
	}

	function lastSeen(ms: number): string {
		if (ms < 1000) return 'just now';
		const s = Math.floor(ms / 1000);
		if (s < 60) return `${s}s ago`;
		const m = Math.floor(s / 60);
		if (m < 60) return `${m}m ago`;
		return `${Math.floor(m / 60)}h ago`;
	}

	function short(id: string): string {
		return id.slice(0, 8);
	}
</script>

<section data-testid="fleet-humans-section">
	<div class="mb-3 flex items-center gap-2">
		<h2 class="text-lg font-semibold tracking-tight text-foreground">Humans</h2>
		<Badge variant="outline" class="rounded-full text-xs text-muted-foreground">
			offer · presence
		</Badge>
	</div>

	{#if capacities.length === 0}
		<div
			class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-10"
		>
			<Users class="size-10 text-muted-foreground/40" />
			<p class="mt-2 text-sm text-muted-foreground">No human capacities</p>
			<p class="text-xs text-muted-foreground">
				Create a capacity with the <span class="font-mono">offer</span> dispatch to pool human
				workers, then enroll members into it.
			</p>
		</div>
	{:else}
		<div class="grid gap-4 sm:grid-cols-2">
			{#each capacities as cap (cap.id)}
				{@const roster = membersFor(cap.id)}
				{@const online = onlineCount(cap.id)}
				<div class="rounded-xl border border-border bg-card p-4" data-testid="human-capacity-card">
					<div class="flex items-start justify-between gap-2">
						<div class="min-w-0">
							<div class="truncate text-sm font-semibold text-foreground">{cap.display_name}</div>
							<div class="truncate font-mono text-xs text-muted-foreground">{cap.path}</div>
						</div>
						<div class="flex shrink-0 items-center gap-2">
							<Badge variant="outline" class="rounded-full">
								{online} / {roster.length} online
							</Badge>
							{#if isAdmin}
								<Button
									variant="outline"
									size="sm"
									class="h-7 gap-1 px-2 text-xs"
									onclick={() => openEnroll(cap)}
									data-testid="human-enroll-btn"
								>
									<UserPlus class="size-3.5" />
									Enroll
								</Button>
							{/if}
						</div>
					</div>

					<div class="mt-3 space-y-1.5">
						{#if roster.length === 0}
							<div class="flex flex-col items-start gap-1.5">
								<p class="text-xs text-muted-foreground">No members enrolled</p>
								{#if isAdmin}
									<Button
										variant="outline"
										size="sm"
										class="h-7 gap-1 px-2 text-xs"
										onclick={() => openEnroll(cap)}
									>
										<UserPlus class="size-3.5" />
										Enroll member
									</Button>
								{/if}
							</div>
						{:else}
							{#each roster as m (m.id)}
								{@const live = presentFor(cap.id, m.member_user_id)}
								<div class="flex items-center justify-between gap-2 text-sm">
									<div class="flex min-w-0 items-center gap-2">
										<StatusDot tone={live?.present ? 'live' : 'idle'} />
										<span class="truncate text-xs text-foreground">
											{#if m.member_display_name}
												{m.member_display_name}
											{:else if m.member_email}
												{m.member_email}
											{:else}
												<span class="font-mono">{short(m.member_user_id)}</span>
											{/if}
										</span>
									</div>
									<div class="flex shrink-0 items-center gap-2">
										<span class="text-xs text-muted-foreground">
											{#if live?.present}
												{lastSeen(live.last_seen_ms_ago)}
											{:else if m.available}
												available (no liveness)
											{:else}
												offline
											{/if}
										</span>
										{#if isAdmin}
											<button
												type="button"
												class="text-muted-foreground/60 hover:text-foreground"
												title="Edit member"
												aria-label="Edit member"
												onclick={() => openEdit(cap, m)}
											>
												<Pencil class="size-3.5" />
											</button>
											<button
												type="button"
												class="text-muted-foreground/60 hover:text-destructive"
												title="Revoke member"
												aria-label="Revoke member"
												onclick={() => handleRevoke(m)}
											>
												<Trash2 class="size-3.5" />
											</button>
										{/if}
									</div>
								</div>
							{/each}
						{/if}
					</div>
				</div>
			{/each}
		</div>
	{/if}

	{#if isAdmin}
		<HumanEnrollSheet
			bind:open={sheetOpen}
			capacityId={sheetCapacityId}
			capacityName={sheetCapacityName}
			member={sheetMember}
			existingMemberIds={memberIdsFor(sheetCapacityId)}
			ondone={() => void poll()}
		/>
	{/if}
</section>
