<!--
  /nets/resource-pool — Generic pool / adapter contention dashboard

  Shows live utilisation + per-hold detail for ANY pool or adapter net (a
  token_pool's `pool-<id>` backing net, a datacenter lease adapter, or the
  standalone prototype `resource-pool-net`). The net id comes from `?net=<id>`;
  it defaults to the prototype so the bare URL keeps working.
    /nets/resource-pool                 → resource-pool-net (prototype)
    /nets/resource-pool?net=pool-<uuid> → a workspace token_pool's backing net

  The view is backend-agnostic: the pool/in_use/done place IDs are shared across
  backends, and per-hold lease fields (unit_id / node / gpu_uuid / alloc_id /
  expiry) render generically from the in_use token. Kill a holder and inject a
  `lease_expired` signal to observe the reap + re-grant path.
-->
<script lang="ts">
	import { page } from '$app/stores';
	import { PoolContentionView } from '$lib/components/petri';
	import { NetWorkbench } from '$lib/components/petri';
	import { Button } from '$lib/components/ui/button';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
	import Network from '@lucide/svelte/icons/network';

	// Net id from `?net=<id>`; defaults to the standalone prototype net.
	const POOL_NET_ID = $derived($page.url.searchParams.get('net') || 'resource-pool-net');

	let viewMode = $state<'dashboard' | 'workbench'>('dashboard');
</script>

<div class="flex h-full flex-col">
	<!-- Page header ── ── ── ── ── ── ── ── ── ── ── ── ── ── ── ── ── -->
	<div class="flex items-center justify-between border-b border-border px-4 py-2 shrink-0">
		<div class="flex items-center gap-3">
			<Button variant="ghost" size="icon-sm" href="/nets">
				<ArrowLeft class="size-4" />
			</Button>
			<span class="font-semibold text-foreground">Resource Pool</span>
			<span class="font-mono text-sm text-muted-foreground">{POOL_NET_ID}</span>
		</div>
		<div class="flex items-center gap-1">
			<Button
				variant={viewMode === 'dashboard' ? 'secondary' : 'ghost'}
				size="sm"
				onclick={() => (viewMode = 'dashboard')}
			>
				<LayoutDashboard class="size-3.5" />
				Dashboard
			</Button>
			<Button
				variant={viewMode === 'workbench' ? 'secondary' : 'ghost'}
				size="sm"
				onclick={() => (viewMode = 'workbench')}
			>
				<Network class="size-3.5" />
				Petri Net
			</Button>
		</div>
	</div>

	<!-- Content area ── ── ── ── ── ── ── ── ── ── ── ── ── ── ── ── ── -->
	<!-- key on the net id so switching `?net=` re-inits the store / workbench. -->
	{#key POOL_NET_ID}
		{#if viewMode === 'dashboard'}
			<div class="flex-1 overflow-y-auto p-6">
				<div class="mx-auto max-w-2xl space-y-6">
					<!-- Live contention view (generic: token pools + datacenter adapters). -->
					<PoolContentionView netId={POOL_NET_ID} />

					<!-- How to read this -->
					<div class="rounded-lg border border-border bg-card p-4 text-sm">
						<p class="mb-2 font-semibold text-foreground">About this view</p>
						<ul class="list-disc space-y-1 pl-4 text-sm text-muted-foreground">
							<li>
								Works for any pool/adapter net — a workspace
								<code class="font-mono text-sm">token_pool</code>'s backing net
								(<code class="font-mono text-sm">pool-&lt;id&gt;</code>), a
								<code class="font-mono text-sm">datacenter</code> lease adapter, or the
								standalone prototype. Pass <code class="font-mono text-sm">?net=&lt;id&gt;</code>
								to target a specific one.
							</li>
							<li>
								The drain bar + counts read the shared
								<code class="font-mono text-sm">pool</code> /
								<code class="font-mono text-sm">in_use</code> places; per-hold rows
								render the typed lease generically (a token pool shows
								<code class="font-mono text-sm">unit_id</code>; a datacenter shows
								<code class="font-mono text-sm">node</code> /
								<code class="font-mono text-sm">gpu_uuid</code> /
								<code class="font-mono text-sm">alloc_id</code> /
								<code class="font-mono text-sm">expiry</code>).
							</li>
							<li>
								Kill a holder + inject a
								<code class="font-mono text-sm">lease_expired</code> signal with the
								matching <code class="font-mono text-sm">grant_id</code> to watch the
								reap + re-grant path. Switch to the Petri Net tab for raw token
								movement.
							</li>
						</ul>
					</div>
				</div>
			</div>
		{:else}
			<!-- Full NetWorkbench view — same as /nets/[id] but pinned to this net. -->
			<div class="relative flex-1 min-h-0">
				<NetWorkbench netId={POOL_NET_ID} />
			</div>
		{/if}
	{/key}
</div>
