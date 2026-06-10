<script lang="ts">
	import { PageShell, PageHeader, FilterPills } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import Server from '@lucide/svelte/icons/server';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import Eye from '@lucide/svelte/icons/eye';
	import OctagonX from '@lucide/svelte/icons/octagon-x';
	import Eraser from '@lucide/svelte/icons/eraser';
	import Flame from '@lucide/svelte/icons/flame';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import {
		listAdminNets,
		killNet,
		purgeNetEvents,
		AdminNetsForbidden,
		type AdminNetRow
	} from '$lib/api/admin-nets';

	const PETRI_URL = '/petri';

	/** A running net firing this many events is almost certainly a runaway. */
	const RUNAWAY_EVENT_THRESHOLD = 5000;

	let nets = $state<AdminNetRow[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let notice = $state<string | null>(null);
	let filter = $state<'all' | 'active' | 'terminal'>('active');
	/** Admin view (kill/purge + event counts) vs read-only engine metadata. */
	let isAdmin = $state(true);
	/** net_ids with an action in flight — disables that row's buttons. */
	let busy = $state<Set<string>>(new Set());

	const statusColors: Record<string, string> = {
		running: 'bg-blue-100 text-blue-700',
		created: 'bg-gray-100 text-gray-700',
		completed: 'bg-green-100 text-green-700',
		cancelled: 'bg-slate-100 text-slate-700',
		failed: 'bg-red-100 text-red-700'
	};

	const isActive = (n: AdminNetRow) => n.status === 'running' || n.status === 'created';

	const filteredNets = $derived(
		filter === 'all'
			? nets
			: filter === 'active'
				? nets.filter(isActive)
				: nets.filter((n) => !isActive(n))
	);

	const isRunaway = (n: AdminNetRow) =>
		isActive(n) && (n.event_count ?? 0) >= RUNAWAY_EVENT_THRESHOLD;

	function formatCount(c: number | undefined | null): string {
		if (c == null) return '—';
		if (c >= 10_000) return `${(c / 1000).toFixed(0)}k`;
		if (c >= 1_000) return `${(c / 1000).toFixed(1)}k`;
		return String(c);
	}

	async function load() {
		loading = true;
		error = null;
		try {
			nets = await listAdminNets();
			isAdmin = true;
		} catch (e) {
			if (e instanceof AdminNetsForbidden) {
				// Non-admin: fall back to the read-only engine metadata listing
				// (same data minus event counts / instance join / actions).
				isAdmin = false;
				try {
					const res = await fetch(`${PETRI_URL}/api/nets/metadata`);
					if (!res.ok) throw new Error(`Engine returned ${res.status}`);
					nets = await res.json();
				} catch (e2) {
					error = e2 instanceof Error ? e2.message : 'Failed to load nets';
					nets = [];
				}
			} else {
				error = e instanceof Error ? e.message : 'Failed to load nets';
				nets = [];
			}
		} finally {
			loading = false;
		}
	}

	function setBusy(netId: string, on: boolean) {
		const next = new Set(busy);
		if (on) next.add(netId);
		else next.delete(netId);
		busy = next;
	}

	async function handleKill(net: AdminNetRow) {
		const detail = isActive(net)
			? 'This terminates the net: releases held leases, emits NetCancelled, cancels its tasks.'
			: 'This removes the net from the engine registry.';
		if (!confirm(`Kill net "${net.net_id}"?\n\n${detail}`)) return;
		setBusy(net.net_id, true);
		try {
			await killNet(net.net_id);
			notice = `Net ${net.net_id} terminated`;
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to kill net';
		} finally {
			setBusy(net.net_id, false);
		}
	}

	async function handlePurge(net: AdminNetRow) {
		if (
			!confirm(
				`Purge ${formatCount(net.event_count)} events of "${net.net_id}" from PETRI_GLOBAL?\n\n` +
					'This permanently deletes the net’s event history (projection rows already written are kept). ' +
					'Only do this for terminal nets whose history you no longer need.'
			)
		)
			return;
		setBusy(net.net_id, true);
		try {
			const res = await purgeNetEvents(net.net_id);
			notice = `Purged ${res.purged_messages} messages for ${net.net_id}`;
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to purge events';
		} finally {
			setBusy(net.net_id, false);
		}
	}

	$effect(() => {
		load();
	});
</script>

<!-- Full-bleed engine-internals list: pinned toolbar band + edge-to-edge rows
     scrolling below it (deliberately not a max-w card list), hence `bleed`. -->
<PageShell width="bleed">
	<div class="flex h-full flex-col">
		<div class="shrink-0 border-b border-border bg-card px-6 py-4">
			<PageHeader
				title="Engine Nets"
				icon={Server}
				subtitle="{filteredNets.length} of {nets.length} nets{isAdmin
					? ''
					: ' · read-only (admin required for actions)'}"
				class="mb-0"
			>
				{#snippet actions()}
					<Button variant="outline" size="sm" onclick={load}>
						<RefreshCw class="size-3.5" />
						Refresh
					</Button>
				{/snippet}
			</PageHeader>
		</div>

		<div class="flex-1 overflow-y-auto">
			<div class="px-6 py-4">
				<FilterPills
					active={filter}
					onSelect={(v) => (filter = v as typeof filter)}
					options={[
						{ value: 'active', label: 'Active' },
						{ value: 'terminal', label: 'Terminal' },
						{ value: 'all', label: 'All' }
					]}
				/>
			</div>
			{#if notice}
				<div class="px-6 pb-2 text-sm text-muted-foreground">{notice}</div>
			{/if}
			{#if loading}
				<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">
					Loading nets from engine...
				</div>
			{:else if error}
				<div class="px-6 py-4 text-sm text-destructive">
					{error}
				</div>
			{:else if filteredNets.length === 0}
				<div
					class="flex flex-col items-center justify-center gap-2 py-12 text-sm text-muted-foreground"
				>
					<Server class="size-8 opacity-30" />
					<p>No {filter === 'all' ? '' : filter} nets found</p>
				</div>
			{:else}
				<div class="divide-y divide-border">
					{#each filteredNets as net (net.net_id)}
						<div
							class="flex items-center justify-between px-6 py-3 transition-colors hover:bg-accent/30"
						>
							<div class="flex min-w-0 items-center gap-3">
								<div class="flex min-w-0 flex-col">
									<span class="flex items-center gap-2 font-mono text-sm">
										<span class="truncate">{net.net_id}</span>
										{#if isRunaway(net)}
											<Badge class="gap-1 bg-red-100 text-red-700">
												<Flame class="size-3" />
												runaway?
											</Badge>
										{/if}
									</span>
									<span class="flex items-center gap-2 text-sm text-muted-foreground">
										{#if net.label}<span class="truncate">{net.label}</span>
											<span>·</span>{/if}
										<span>{net.in_memory ? 'in memory' : 'hibernated'}</span>
										{#if isAdmin}
											<span>·</span>
											<span
												class={isRunaway(net) ? 'font-medium text-red-600' : ''}
												title="messages on petri.events.{net.net_id}.> in PETRI_GLOBAL"
											>
												{formatCount(net.event_count)} events
											</span>
										{/if}
										{#if net.instance_id}
											<a
												href="/instances/{net.instance_id}"
												class="inline-flex items-center gap-0.5 hover:text-foreground hover:underline"
											>
												instance <ExternalLink class="size-3" />
											</a>
										{/if}
									</span>
								</div>
							</div>
							<div class="flex shrink-0 items-center gap-2">
								<Badge class={statusColors[net.status] ?? 'bg-gray-100 text-gray-700'}>
									{net.status}
								</Badge>
								<Button variant="ghost" size="icon-sm" href="/nets/{net.net_id}">
									<Eye class="size-3.5" />
								</Button>
								{#if isAdmin}
									{#if !isActive(net)}
										<Button
											variant="ghost"
											size="icon-sm"
											disabled={busy.has(net.net_id)}
											title="Purge this net's events from PETRI_GLOBAL"
											onclick={() => handlePurge(net)}
										>
											<Eraser class="size-3.5 text-muted-foreground hover:text-destructive" />
										</Button>
									{/if}
									<Button
										variant="ghost"
										size="icon-sm"
										disabled={busy.has(net.net_id)}
										title={isActive(net)
											? 'Kill switch: terminate this net'
											: 'Remove this net from the engine'}
										onclick={() => handleKill(net)}
									>
										<OctagonX class="size-3.5 text-muted-foreground hover:text-destructive" />
									</Button>
								{/if}
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	</div>
</PageShell>
