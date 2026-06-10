<script lang="ts">
	// Per-pool human roster — the member list ported out of the retired
	// HumanCapacitySection, scoped to ONE consent-presence capacity (the pool
	// detail page's Members tab for human pools).
	//
	// "Live presence" is the in-memory admission snapshot from
	// `GET /api/v1/human-presence` (which members mekhan currently considers in
	// the pool), joined to the durable `roster_members` enrollment list. A
	// member can be enrolled-but-offline (no presence row, or `present:false`).
	//
	// Workspace admins get the enrollment controls — enroll a member, edit
	// caps/concurrency/availability, or revoke — gated on `auth.isWorkspaceAdmin`
	// (hidden for everyone else). They drive a single shared HumanEnrollSheet.
	// The page band's "Enroll member" forwards into the exported `openEnroll()`.
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
	import { Button } from '$lib/components/ui/button';
	import Users from '@lucide/svelte/icons/users';
	import UserPlus from '@lucide/svelte/icons/user-plus';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { toast } from 'svelte-sonner';

	type Props = {
		/** The consent-presence capacity (human pool) whose roster this lists. */
		capacityId: string;
		/** Display name of that capacity (enroll-sheet header copy only). */
		capacityName?: string;
	};
	let { capacityId, capacityName = '' }: Props = $props();

	const isAdmin = $derived(auth.isWorkspaceAdmin);

	let presence = $state<HumanPresenceSnapshot[]>([]);
	let members = $state<RosterMemberSummary[]>([]);

	// ── Enroll/edit sheet state (single shared instance) ───────────────────────
	let sheetOpen = $state(false);
	let sheetMember = $state<RosterMemberSummary | null>(null);

	async function poll() {
		const [p, m] = await Promise.all([
			getHumanPresence().catch(() => [] as HumanPresenceSnapshot[]),
			listRoster(capacityId).catch(() => [] as RosterMemberSummary[])
		]);
		presence = p.filter((s) => s.capacity_id === capacityId);
		// Defensive re-scope — `listRoster(capacityId)` already filters server-side.
		members = m.filter((x) => x.capacity_id === capacityId);
	}

	$effect(() => {
		void capacityId;
		void poll();
		const t = setInterval(() => void poll(), 5000);
		return () => clearInterval(t);
	});

	/** Open the enroll flow (member picker). Exported so the page band's single
	 *  "Enroll member" action can forward here via `bind:this`. */
	export function openEnroll() {
		sheetMember = null;
		sheetOpen = true;
	}

	function openEdit(m: RosterMemberSummary) {
		sheetMember = m;
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

	const onlineCount = $derived(presence.filter((p) => p.present).length);

	function presentFor(memberId: string): HumanPresenceSnapshot | undefined {
		return presence.find((p) => p.member_user_id === memberId);
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

<div class="space-y-4" data-testid="pool-members-humans">
	<!-- Header row: title + live count on the left, the enroll action (admin) on
		 the right — mirrors the runner roster's header row. -->
	<div class="flex flex-wrap items-center justify-between gap-3 border-b border-border pb-3">
		<div class="flex items-center gap-2">
			<h3 class="text-sm font-semibold text-foreground">Members</h3>
			<span class="text-sm tabular-nums text-muted-foreground">
				{onlineCount} / {members.length} online
			</span>
		</div>
		{#if isAdmin}
			<Button
				variant="default"
				size="sm"
				class="gap-1.5"
				onclick={openEnroll}
				data-testid="human-enroll-btn"
			>
				<UserPlus class="size-4" />
				Enroll member
			</Button>
		{/if}
	</div>

	{#if members.length === 0}
		<div
			class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-10"
		>
			<Users class="size-10 text-muted-foreground/40" />
			<p class="mt-2 text-sm text-muted-foreground">No members enrolled</p>
			<p class="text-xs text-muted-foreground">
				Enroll workspace members into this pool — they claim offered tasks to accept them.
			</p>
			{#if isAdmin}
				<Button variant="outline" size="sm" class="mt-3 gap-1.5" onclick={openEnroll}>
					<UserPlus class="size-3.5" />
					Enroll member
				</Button>
			{/if}
		</div>
	{:else}
		<div class="space-y-2">
			{#each members as m (m.id)}
				{@const live = presentFor(m.member_user_id)}
				<div
					class="group flex items-center justify-between gap-2 rounded-lg border border-border bg-card px-4 py-3 transition-colors hover:bg-accent/40"
					data-testid="human-member-{m.id}"
				>
					<div class="flex min-w-0 items-center gap-2.5">
						<StatusDot tone={live?.present ? 'live' : 'idle'} />
						<div class="min-w-0">
							<p class="truncate text-sm text-foreground">
								{#if m.member_display_name}
									{m.member_display_name}
								{:else if m.member_email}
									{m.member_email}
								{:else}
									<span class="font-mono">{short(m.member_user_id)}</span>
								{/if}
							</p>
							{#if m.member_display_name && m.member_email}
								<p class="truncate text-xs text-muted-foreground">{m.member_email}</p>
							{/if}
						</div>
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
								onclick={() => openEdit(m)}
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
		</div>
	{/if}
</div>

{#if isAdmin}
	<HumanEnrollSheet
		bind:open={sheetOpen}
		{capacityId}
		{capacityName}
		member={sheetMember}
		existingMemberIds={members.map((m) => m.member_user_id)}
		ondone={() => void poll()}
	/>
{/if}
