<script lang="ts">
	import { goto } from '$app/navigation';
	import { listTaskInbox, claimTask } from '$lib/api/client';
	import { getMyEnrollments, setAvailability, type RosterMemberDetail } from '$lib/api/roster';
	import { listCapacities, type CapacitySummary } from '$lib/api/capacities';
	import { connectSse, type SseConnection } from '$lib/net/sse';
	import { authFetch } from '$lib/auth/fetch';
	import type { HumanTask } from '$lib/types/tasks';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import Inbox from '@lucide/svelte/icons/inbox';
	import Hand from '@lucide/svelte/icons/hand';
	import ClipboardList from '@lucide/svelte/icons/clipboard-list';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';

	const POLL_MS = 4000;

	let enrollments = $state<RosterMemberDetail[]>([]);
	let capacityNames = $state<Record<string, string>>({});
	let tasks = $state<HumanTask[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	// task_ids with an in-flight claim — their button shows a pending state until
	// the projection flips the row to `claimed` (or it leaves the offered set).
	let claiming = $state<Set<string>>(new Set());
	let togglingCaps = $state<Set<string>>(new Set());

	const offered = $derived(tasks.filter((t) => t.status === 'offered'));
	// Unpooled tasks bound to no capacity — open to anyone in the workspace.
	const open = $derived(tasks.filter((t) => t.status === 'pending'));
	const claimed = $derived(tasks.filter((t) => t.status === 'claimed'));

	function capName(id: string): string {
		return capacityNames[id] ?? id.slice(0, 8);
	}

	function formatDate(s?: string | null): string {
		return s ? new Date(s).toLocaleString() : '—';
	}

	async function refresh() {
		try {
			const res = await listTaskInbox();
			tasks = res.tasks;
			// Drop any claim-pending markers whose task is no longer claimable
			// (left the offered/open buckets — claimed, completed, or gone).
			const stillClaimable = new Set(
				res.tasks
					.filter((t) => t.status === 'offered' || t.status === 'pending')
					.map((t) => t.task_id)
			);
			claiming = new Set([...claiming].filter((id) => stillClaimable.has(id)));
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	async function refreshEnrollments() {
		try {
			enrollments = await getMyEnrollments();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	async function toggleAvailability(m: RosterMemberDetail) {
		const next = !m.available;
		togglingCaps = new Set(togglingCaps).add(m.capacity_id);
		try {
			await setAvailability(m.capacity_id, next);
			// Optimistic: reflect immediately, then reconcile from the server.
			enrollments = enrollments.map((e) =>
				e.capacity_id === m.capacity_id ? { ...e, available: next } : e
			);
			await refreshEnrollments();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			const s = new Set(togglingCaps);
			s.delete(m.capacity_id);
			togglingCaps = s;
		}
	}

	async function onClaim(task: HumanTask) {
		claiming = new Set(claiming).add(task.task_id);
		try {
			await claimTask(task.task_id);
			// 202 — the authoritative `claimed` flip arrives via the pool-net
			// projection. Poll until the row leaves `offered`, then take the user
			// straight into the task so they can start working on it.
			for (let i = 0; i < 8; i++) {
				await new Promise((r) => setTimeout(r, 500));
				await refresh();
				const mine = tasks.find((t) => t.task_id === task.task_id);
				if (mine && mine.status !== 'offered') break;
			}
			await goto(`/tasks/${task.task_id}`);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
			const s = new Set(claiming);
			s.delete(task.task_id);
			claiming = s;
		}
	}

	$effect(() => {
		let alive = true;
		(async () => {
			loading = true;
			const caps = await listCapacities().catch(() => [] as CapacitySummary[]);
			if (!alive) return;
			capacityNames = Object.fromEntries(caps.map((c) => [c.id, c.display_name]));
			await Promise.all([refreshEnrollments(), refresh()]);
			loading = false;
		})();

		// Holding the task stream open is the human's SESSION liveness source.
		// The server publishes `human.{member}.presence` on connect and on every
		// 10s ping; the presence controller renews `session`-mode availability
		// (TTL 45s) off those heartbeats. Without an open stream a `session`
		// toggle would be admitted then reaped by the TTL sweep ~45s later — i.e.
		// the inbox toggle silently lapses. (`sticky` mode is admitted on intent
		// alone and is unaffected.) Live task events also refresh the inbox
		// faster than the poll fallback below.
		let sse: SseConnection | null = connectSse('/api/v1/tasks/stream', {
			fetchImpl: authFetch,
			maxRetries: 5,
			initialRetryMs: 1000,
			// Terminal client error (auth/gone): stop cleanly, the poll still runs.
			onTerminal: () => {
				sse?.close();
				sse = null;
			},
			onEvent: ({ event }) => {
				if (
					event === 'task_created' ||
					event === 'task_completed' ||
					event === 'task_failed' ||
					event === 'task_cancelled'
				) {
					refresh();
				}
			}
		});

		const timer = setInterval(() => {
			if (!alive) return;
			refresh();
		}, POLL_MS);

		return () => {
			alive = false;
			clearInterval(timer);
			sse?.close();
		};
	});
</script>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-5xl px-6 py-8 animate-rise">
		<div class="mb-6 flex items-start justify-between gap-3">
			<div>
				<h1 class="text-2xl font-semibold tracking-tight text-foreground">Inbox</h1>
				<p class="mt-1 text-sm text-muted-foreground">
					Tasks offered to you, and the work you've claimed
				</p>
			</div>
			<Button variant="ghost" size="sm" onclick={refresh} data-testid="inbox-refresh">
				<RefreshCw class="size-4" />
			</Button>
		</div>

		{#if error}
			<div
				class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800"
			>
				{error}
			</div>
		{/if}

		<!-- Availability — one toggle per enrolled human capacity -->
		<div class="mb-6 rounded-xl border border-border bg-card p-4">
			<div class="mb-3 text-sm font-semibold text-foreground">Availability</div>
			{#if enrollments.length === 0}
				<p class="text-sm text-muted-foreground">
					You aren't enrolled in any human capacity yet. An admin enrolls members on the
					<a class="underline" href="/fleet">Fleet</a> page.
				</p>
			{:else}
				<div class="space-y-2">
					{#each enrollments as m (m.capacity_id)}
						<div class="flex items-center justify-between gap-3">
							<div class="min-w-0">
								<div class="truncate text-sm font-medium text-foreground">
									{capName(m.capacity_id)}
								</div>
								<div class="text-xs text-muted-foreground">
									{#if m.available}
										Available since {formatDate(m.available_since)}
									{:else}
										Offline
									{/if}
								</div>
							</div>
							<Button
								variant={m.available ? 'default' : 'outline'}
								size="sm"
								disabled={togglingCaps.has(m.capacity_id)}
								onclick={() => toggleAvailability(m)}
								data-testid="availability-toggle"
							>
								{#if togglingCaps.has(m.capacity_id)}
									…
								{:else if m.available}
									Available
								{:else}
									Go available
								{/if}
							</Button>
						</div>
					{/each}
				</div>
			{/if}
		</div>

		{#if loading}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Loading…
			</div>
		{:else}
			<!-- Offered to you -->
			<div class="mb-6">
				<div class="mb-2 flex items-center gap-2 text-sm font-semibold text-foreground">
					<Hand class="size-4 text-muted-foreground" />
					Offered to you
					{#if offered.length > 0}
						<Badge variant="outline" class="rounded-full">{offered.length}</Badge>
					{/if}
				</div>
				{#if offered.length === 0}
					<div
						class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-10"
					>
						<Inbox class="size-8 text-muted-foreground/40" />
						<p class="mt-2 text-sm text-muted-foreground">No open offers</p>
						<p class="text-xs text-muted-foreground">
							Offers appear here when a workflow reaches a human task bound to a capacity you're
							available for
						</p>
					</div>
				{:else}
					<div class="space-y-2">
						{#each offered as task (task.task_id)}
							<div
								class="flex items-start justify-between gap-3 rounded-xl border border-border bg-card p-4 transition hover:border-primary/40"
							>
								<div class="min-w-0 flex-1">
									<div class="truncate text-sm font-semibold leading-snug text-foreground">
										{task.title}
									</div>
									<div class="mt-1 text-xs text-muted-foreground">
										Offered {formatDate(task.created_at)}
										{#if task.process_id}
											· <span class="font-mono">{task.process_id}</span>
										{/if}
									</div>
								</div>
								<Button
									size="sm"
									disabled={claiming.has(task.task_id)}
									onclick={() => onClaim(task)}
									data-testid="claim-button"
								>
									{claiming.has(task.task_id) ? 'Claiming…' : 'Claim'}
								</Button>
							</div>
						{/each}
					</div>
				{/if}
			</div>

			<!-- Open to anyone — unpooled tasks (no capacity), claimable by any
			     workspace member. Claiming is a soft assign; anyone can still
			     complete an unclaimed one. -->
			{#if open.length > 0}
				<div class="mb-6">
					<div class="mb-2 flex items-center gap-2 text-sm font-semibold text-foreground">
						<ClipboardList class="size-4 text-muted-foreground" />
						Open to anyone
						<Badge variant="outline" class="rounded-full">{open.length}</Badge>
					</div>
					<div class="space-y-2">
						{#each open as task (task.task_id)}
							<div
								class="flex items-start justify-between gap-3 rounded-xl border border-border bg-card p-4 transition hover:border-primary/40"
							>
								<div class="min-w-0 flex-1">
									<div class="truncate text-sm font-semibold leading-snug text-foreground">
										{task.title}
									</div>
									<div class="mt-1 text-xs text-muted-foreground">
										Anyone in your workspace can take this on · {formatDate(task.created_at)}
									</div>
								</div>
								<Button
									size="sm"
									variant="outline"
									disabled={claiming.has(task.task_id)}
									onclick={() => onClaim(task)}
									data-testid="claim-button"
								>
									{claiming.has(task.task_id) ? 'Claiming…' : 'Claim'}
								</Button>
							</div>
						{/each}
					</div>
				</div>
			{/if}

			<!-- Claimed by you -->
			<div>
				<div class="mb-2 flex items-center gap-2 text-sm font-semibold text-foreground">
					In progress
					{#if claimed.length > 0}
						<Badge variant="outline" class="rounded-full">{claimed.length}</Badge>
					{/if}
				</div>
				{#if claimed.length === 0}
					<p class="px-1 text-sm text-muted-foreground">Nothing in progress</p>
				{:else}
					<div class="space-y-2">
						{#each claimed as task (task.task_id)}
							<a
								href="/tasks/{task.task_id}"
								class="group block rounded-xl border border-border bg-card p-4 transition hover:border-primary/40 hover:shadow-md"
							>
								<div class="flex items-start justify-between gap-3">
									<div class="min-w-0 flex-1">
										<div class="truncate text-sm font-semibold leading-snug text-foreground">
											{task.title}
										</div>
										<div class="mt-1 text-xs text-muted-foreground">
											Claimed {formatDate(task.created_at)}
										</div>
									</div>
									<Badge
										variant="outline"
										class="shrink-0 rounded-full border-sky-200 bg-sky-50 text-sky-700"
									>
										Claimed
									</Badge>
								</div>
							</a>
						{/each}
					</div>
				{/if}
			</div>
		{/if}
	</div>
</div>
