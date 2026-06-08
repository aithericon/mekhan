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
	import type { CapacitySummary } from '$lib/api/capacities';
	import {
		getHumanPresence,
		listRoster,
		type HumanPresenceSnapshot,
		type RosterMemberSummary
	} from '$lib/api/roster';
	import StatusDot from '$lib/components/fleet/StatusDot.svelte';
	import { Badge } from '$lib/components/ui/badge';
	import Users from '@lucide/svelte/icons/users';

	let { capacities }: { capacities: CapacitySummary[] } = $props();

	let presence = $state<HumanPresenceSnapshot[]>([]);
	let members = $state<RosterMemberSummary[]>([]);

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

	function membersFor(capId: string): RosterMemberSummary[] {
		return members.filter((m) => m.capacity_id === capId);
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
						<Badge variant="outline" class="shrink-0 rounded-full">
							{online} / {roster.length} online
						</Badge>
					</div>

					<div class="mt-3 space-y-1.5">
						{#if roster.length === 0}
							<p class="text-xs text-muted-foreground">No members enrolled</p>
						{:else}
							{#each roster as m (m.id)}
								{@const live = presentFor(cap.id, m.member_user_id)}
								<div class="flex items-center justify-between gap-2 text-sm">
									<div class="flex min-w-0 items-center gap-2">
										<StatusDot tone={live?.present ? 'live' : 'idle'} />
										<span class="truncate font-mono text-xs text-foreground">
											{short(m.member_user_id)}
										</span>
									</div>
									<span class="shrink-0 text-xs text-muted-foreground">
										{#if live?.present}
											{lastSeen(live.last_seen_ms_ago)}
										{:else if m.available}
											available (no liveness)
										{:else}
											offline
										{/if}
									</span>
								</div>
							{/each}
						{/if}
					</div>
				</div>
			{/each}
		</div>
	{/if}
</section>
