<script lang="ts">
	import { PageShell, PageHeader, FilterPills } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import Server from '@lucide/svelte/icons/server';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Eye from '@lucide/svelte/icons/eye';

	const PETRI_URL = '/petri';

	type NetMeta = {
		net_id: string;
		status: string;
		in_memory: boolean;
	};

	let nets = $state<NetMeta[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let filter = $state<'all' | 'active' | 'terminal'>('active');

	const statusColors: Record<string, string> = {
		running: 'bg-blue-100 text-blue-700',
		created: 'bg-gray-100 text-gray-700',
		completed: 'bg-green-100 text-green-700',
		cancelled: 'bg-slate-100 text-slate-700',
		failed: 'bg-red-100 text-red-700'
	};

	const filteredNets = $derived(
		filter === 'all'
			? nets
			: filter === 'active'
				? nets.filter((n) => n.status === 'running' || n.status === 'created')
				: nets.filter((n) => n.status !== 'running' && n.status !== 'created')
	);

	async function load() {
		loading = true;
		error = null;
		try {
			const res = await fetch(`${PETRI_URL}/api/nets/metadata`);
			if (!res.ok) throw new Error(`Engine returned ${res.status}`);
			nets = await res.json();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load nets';
			nets = [];
		} finally {
			loading = false;
		}
	}

	async function deleteNet(netId: string) {
		if (!confirm(`Delete net "${netId}"? This removes it from the engine registry.`)) return;
		try {
			await fetch(`${PETRI_URL}/api/nets/${netId}`, { method: 'DELETE' });
			nets = nets.filter((n) => n.net_id !== netId);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to delete net';
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
				subtitle="{filteredNets.length} of {nets.length} nets"
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
			{#if loading}
				<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">
					Loading nets from engine...
				</div>
			{:else if error}
				<div class="px-6 py-4 text-sm text-destructive">
					{error}
				</div>
			{:else if filteredNets.length === 0}
				<div class="flex flex-col items-center justify-center gap-2 py-12 text-sm text-muted-foreground">
					<Server class="size-8 opacity-30" />
					<p>No {filter === 'all' ? '' : filter} nets found</p>
				</div>
			{:else}
				<div class="divide-y divide-border">
					{#each filteredNets as net (net.net_id)}
						<div
							class="flex items-center justify-between px-6 py-3 transition-colors hover:bg-accent/30"
						>
							<div class="flex items-center gap-3">
								<div class="flex flex-col">
									<span class="font-mono text-sm">{net.net_id}</span>
									<span class="text-sm text-muted-foreground">
										{net.in_memory ? 'in memory' : 'hibernated'}
									</span>
								</div>
							</div>
							<div class="flex items-center gap-2">
								<Badge
									class={statusColors[net.status] ?? 'bg-gray-100 text-gray-700'}
								>
									{net.status}
								</Badge>
								<Button variant="ghost" size="icon-sm" href="/nets/{net.net_id}">
									<Eye class="size-3.5" />
								</Button>
								<Button
									variant="ghost"
									size="icon-sm"
									onclick={() => deleteNet(net.net_id)}
								>
									<Trash2 class="size-3.5 text-muted-foreground hover:text-destructive" />
								</Button>
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	</div>
</PageShell>
