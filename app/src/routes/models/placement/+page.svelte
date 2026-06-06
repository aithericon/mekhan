<script lang="ts">
	// PLACEMENT tab — the autoscaler's view of the pool (docs/29 §6', docs/31
	// Loop 1). Two read models, each a resource ⋈ its reconciliation row:
	//   Placement policies — model_policy resources ⋈ model_replicas (per-model
	//     desired/observed count + residency zone + last error).
	//   Node pools — node_pool resources ⋈ node_replicas (desired/observed nodes
	//     + slots). Node provisioning (Nomad) is deferred; surfaced read-only.
	import {
		listModelReplicas,
		listNodeReplicas,
		type ModelReplicaRow,
		type NodeReplicaRow
	} from '$lib/api/models';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import { statusTone } from '$lib/components/fleet/model-pool';

	let policies = $state<ResourceSummary[]>([]);
	let pools = $state<ResourceSummary[]>([]);
	let modelReplicas = $state<ModelReplicaRow[]>([]);
	let nodeReplicas = $state<NodeReplicaRow[]>([]);
	let error = $state<string | null>(null);

	async function poll() {
		try {
			const [pol, pl, mr, nr] = await Promise.all([
				listResources({ resource_type: 'model_policy', perPage: 100 }),
				listResources({ resource_type: 'node_pool', perPage: 100 }),
				listModelReplicas(),
				listNodeReplicas()
			]);
			policies = pol.items;
			pools = pl.items;
			modelReplicas = mr;
			nodeReplicas = nr;
			error = null;
		} catch (err) {
			error = err instanceof Error ? err.message : 'Failed to load placement';
		}
	}

	$effect(() => {
		void poll();
		const t = setInterval(() => void poll(), 5000);
		return () => clearInterval(t);
	});

	const replicaFor = (policyId: string) =>
		modelReplicas.find((r) => r.policy_resource_id === policyId);
	const nodeReplicaFor = (poolId: string) => nodeReplicas.find((r) => r.pool_resource_id === poolId);
</script>

<div class="space-y-6" data-testid="models-placement">
	<div class="flex items-baseline gap-3">
		<h2 class="text-base font-semibold tracking-tight text-foreground">Placement</h2>
		<span class="text-sm text-muted-foreground">autoscaler policies + node pools</span>
	</div>

	{#if error}
		<div
			class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-2 text-sm text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200"
		>
			{error}
		</div>
	{/if}

	<!-- POLICIES — per-model demand/placement (model_policy + model_replicas) -->
	<div class="space-y-2">
		<h3 class="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
			Placement policies
		</h3>
		{#if policies.length === 0}
			<p class="text-sm text-muted-foreground/70">No model policies.</p>
			<p class="text-sm text-muted-foreground/70">
				Create a <a
					href="/resources"
					class="font-medium text-foreground underline underline-offset-2 hover:text-primary"
					>model_policy resource</a
				> to give the autoscaler per-model desired counts.
			</p>
		{:else}
			<div class="grid gap-2 sm:grid-cols-2">
				{#each policies as p (p.id)}
					{@const r = replicaFor(p.id)}
					<div class="rounded-lg border border-border/60 bg-card p-2.5 text-sm">
						<div class="flex items-center justify-between">
							<span class="font-medium text-foreground">{p.display_name || p.path}</span>
							{#if r}<span class="text-sm {statusTone(r.status)}">{r.status}</span>{/if}
						</div>
						<div class="mt-0.5 text-sm text-muted-foreground">
							{#if r}
								desired {r.desired_count} · observed {r.observed_count}
								{#if r.residency_zone}· zone {r.residency_zone}{/if}
								{#if r.last_error}<span class="text-red-600 dark:text-red-400"> · {r.last_error}</span
									>{/if}
							{:else}
								no replica row yet (autoscaler creates it on first reconcile)
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>

	<!-- NODE POOLS — engine capacity (node_pool + node_replicas). -->
	<div class="space-y-2">
		<h3 class="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
			Node pools
			<span class="ml-1 font-normal normal-case text-muted-foreground/60">
				(engine capacity — provisioning deferred)
			</span>
		</h3>
		{#if pools.length === 0}
			<p class="text-sm text-muted-foreground/70">No node pools.</p>
			<p class="text-sm text-muted-foreground/70">
				Create a <a
					href="/resources"
					class="font-medium text-foreground underline underline-offset-2 hover:text-primary"
					>node_pool resource</a
				> to declare engine capacity for the autoscaler to fill.
			</p>
		{:else}
			<div class="grid gap-2 sm:grid-cols-2">
				{#each pools as p (p.id)}
					{@const r = nodeReplicaFor(p.id)}
					<div class="rounded-lg border border-border/60 bg-card p-2.5 text-sm">
						<div class="flex items-center justify-between">
							<span class="font-medium text-foreground">{p.display_name || p.path}</span>
							{#if r}<span class="text-sm {statusTone(r.status)}">{r.status}</span>{/if}
						</div>
						<div class="mt-0.5 text-sm text-muted-foreground">
							{#if r}
								desired {r.desired_nodes} nodes · observed {r.observed_nodes} · slots {r.observed_slots}
								{#if r.residency_zone}· zone {r.residency_zone}{/if}
							{:else}
								no replica row yet
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
