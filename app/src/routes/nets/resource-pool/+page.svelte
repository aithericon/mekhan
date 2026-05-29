<!--
  /nets/resource-pool — Resource Pool Contention Dashboard

  A dedicated view for the resource-pool-net Petri net, showing live pool
  utilisation and per-hold detail. Reachable from the sidebar or directly at:
    http://localhost:15173/nets/resource-pool

  To demo the contention showcase:
    1. Deploy the pool net:
       cargo run -p aithericon-sdk --example resource_pool_net \
         -- --deploy --net-id resource-pool-net
    2. Keep this page open and watch pool drain live as instance nets claim GPUs.
    3. Kill a running instance to observe the lease-reap + re-grant path.
    4. The NetWorkbench link (Eye icon in PoolContentionView header) opens the
       full Petri canvas at /nets/resource-pool-net for the raw net view.
-->
<script lang="ts">
	import { PoolContentionView } from '$lib/components/petri';
	import { NetWorkbench } from '$lib/components/petri';
	import { Button } from '$lib/components/ui/button';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
	import Network from '@lucide/svelte/icons/network';

	const POOL_NET_ID = 'resource-pool-net';

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
	{#if viewMode === 'dashboard'}
		<div class="flex-1 overflow-y-auto p-6">
			<div class="mx-auto max-w-2xl space-y-6">
				<!-- Live contention view -->
				<PoolContentionView netId={POOL_NET_ID} />

				<!-- How to demo -->
				<div class="rounded-lg border border-border bg-card p-4 text-sm">
					<p class="mb-2 font-semibold text-foreground">Demo the showcase scenario</p>
					<ol class="list-decimal space-y-1 pl-4 text-sm text-muted-foreground">
						<li>
							Deploy the pool net once:
							<code class="font-mono text-sm text-foreground">
								cargo run -p aithericon-sdk --example resource_pool_net -- --deploy --net-id resource-pool-net
							</code>
						</li>
						<li>
							Start 4 instance nets that bridge claims into
							<code class="font-mono text-sm">claim_inbox</code>. Watch the pool drain
							2→0 as the first two are granted and the others queue.
						</li>
						<li>
							Kill a running instance to trigger lease reap:
							inject a <code class="font-mono text-sm">lease_expired</code> signal with
							the matching <code class="font-mono text-sm">grant_id</code> into the pool net.
							The freed GPU is immediately re-granted to a waiting instance.
						</li>
						<li>
							Switch to the Petri Net tab to watch raw token movement in NetWorkbench.
						</li>
					</ol>
				</div>

				<!--
				  TODO(M3): Instance contention table — once M3 compiler lowering is
				  deployed, list running instances here and mark which ones have
				  p_{nodeId}_claim_out tokens (claim sent) with empty
				  p_{nodeId}_grant_inbox (no grant yet = waiting). The NodeRuntimeBadge
				  `awaitingResource` prop is ready to receive this signal.
				-->
				<div class="rounded-lg border border-dashed border-border p-4 text-sm text-muted-foreground">
					<p class="font-medium">Per-instance contention overlay — TODO (M3)</p>
					<p class="mt-1 text-sm">
						After M3 compiler lowering is deployed, instance nets will contain
						<code class="font-mono text-sm">p_&lt;nodeId&gt;_claim_out</code> and
						<code class="font-mono text-sm">p_&lt;nodeId&gt;_grant_inbox</code> places.
						Query their token counts from the instance net marking and set
						<code class="font-mono text-sm">awaitingResource</code> on each
						NodeRuntimeBadge to show the "Waiting for resource" (purple hourglass) overlay
						on graph nodes that have emitted a claim but not yet received a grant.
					</p>
				</div>
			</div>
		</div>
	{:else}
		<!-- Full NetWorkbench view — same as /nets/[id] but pinned to pool net -->
		<div class="relative flex-1 min-h-0">
			<NetWorkbench netId={POOL_NET_ID} />
		</div>
	{/if}
</div>
