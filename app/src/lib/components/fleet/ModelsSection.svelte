<script lang="ts">
	// The MODELS control-plane section (docs/28-31). Surfaces the self-hosted
	// model pool that bypasses the engine net: the live per-node engine inventory
	// (GET /api/v1/fleet/engines), the operator-curated model set (GET /api/v1/models),
	// the placement/demand policies (model_policy + model_replicas), and the node
	// pools (node_pool + node_replicas). Engine cards carry load/unload actions
	// that publish a ModelCommand to the runner's model agent (vLLM admin or the
	// Ollama Metal runtime) — control plane only, never inference.
	import { Button } from '$lib/components/ui/button';
	import Cpu from '@lucide/svelte/icons/cpu';
	import Search from '@lucide/svelte/icons/search';
	import {
		listFleetEngines,
		listLoadedModels,
		listModelReplicas,
		listNodeReplicas,
		publishModelCommand,
		baseCommand,
		type FleetEnginesResponse,
		type ModelSetView,
		type ModelReplicaRow,
		type NodeReplicaRow
	} from '$lib/api/models';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import ModelBrowser from './ModelBrowser.svelte';

	let engines = $state<FleetEnginesResponse>({ headroom_from_router: false, nodes: [] });
	let models = $state<ModelSetView[]>([]);
	let modelReplicas = $state<ModelReplicaRow[]>([]);
	let nodeReplicas = $state<NodeReplicaRow[]>([]);
	let policies = $state<ResourceSummary[]>([]);
	let pools = $state<ResourceSummary[]>([]);
	let error = $state<string | null>(null);
	let busy = $state<string | null>(null);
	let loadInputs = $state<Record<string, string>>({});

	// Model browser: opened against a specific runner; "Provision" pulls the
	// chosen model onto it.
	let browserOpen = $state(false);
	let browserRunner = $state<string | null>(null);

	async function poll() {
		try {
			const [e, m, mr, nr, pol, pl] = await Promise.all([
				listFleetEngines(),
				listLoadedModels(),
				listModelReplicas(),
				listNodeReplicas(),
				listResources({ resource_type: 'model_policy', perPage: 100 }),
				listResources({ resource_type: 'node_pool', perPage: 100 })
			]);
			engines = e;
			models = m;
			modelReplicas = mr;
			nodeReplicas = nr;
			policies = pol.items;
			pools = pl.items;
			error = null;
		} catch (err) {
			error = err instanceof Error ? err.message : 'Failed to load the model pool';
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
	const shortId = (id: string) => id.slice(0, 8);

	async function act(runnerId: string, verb: 'load' | 'unload' | 'pull', modelId: string) {
		if (!modelId) return;
		busy = `${runnerId}:${modelId}:${verb}`;
		try {
			await publishModelCommand(runnerId, baseCommand(verb, modelId));
			// Fire-and-forget: give the agent a moment to apply + re-publish its
			// catalog. A pull downloads weights (can be slow); the agent re-publishes
			// when done and the next 5s poll surfaces it under "ready to load".
			await new Promise((r) => setTimeout(r, verb === 'pull' ? 800 : 1500));
			await poll();
		} catch (err) {
			error = err instanceof Error ? err.message : 'Command failed';
		} finally {
			busy = null;
		}
	}

	function openBrowser(runnerId: string) {
		browserRunner = runnerId;
		browserOpen = true;
	}

	/** Provision (pull) the browser-selected model onto the open browser's runner. */
	function onProvision(provisionId: string) {
		if (browserRunner) void act(browserRunner, 'pull', provisionId);
	}

	function statusTone(s: string): string {
		if (s === 'active' || s === 'loaded') return 'text-emerald-600 dark:text-emerald-400';
		if (s === 'failed') return 'text-red-600 dark:text-red-400';
		if (s === 'stopped' || s === 'unloaded') return 'text-muted-foreground';
		return 'text-amber-600 dark:text-amber-400';
	}
</script>

<section data-testid="models-section" class="space-y-4">
	<div class="flex items-baseline gap-3">
		<h2 class="text-sm font-semibold tracking-tight text-foreground">Models</h2>
		<span class="text-sm text-muted-foreground">
			self-hosted pool — inference bypasses the engine net (HTTP router)
		</span>
		{#if !engines.headroom_from_router}
			<span class="text-xs text-muted-foreground/70"
				>headroom = full budget (router poll unconfigured)</span
			>
		{/if}
	</div>

	{#if error}
		<div
			class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-2 text-sm text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200"
		>
			{error}
		</div>
	{/if}

	<!-- ENGINES — live per-node inventory + load/unload actions -->
	<div class="space-y-2">
		<h3 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">Engines</h3>
		{#if engines.nodes.length === 0}
			<div
				class="flex flex-col items-center gap-2 rounded-lg border border-dashed border-border/60 py-8 text-sm text-muted-foreground"
			>
				<Cpu class="size-8 text-muted-foreground/40" />
				No model-server runners. Enrol a runner with a <code>[model_agent]</code> backend (vLLM or Ollama).
			</div>
		{:else}
			<div class="grid gap-3 sm:grid-cols-2">
				{#each engines.nodes as node (node.runner_id)}
					<div class="rounded-lg border border-border/60 bg-card p-3" data-testid="engine-card">
						<div class="mb-2 flex items-center justify-between">
							<span class="font-mono text-xs text-muted-foreground">runner {shortId(node.runner_id)}</span>
							<span class="text-xs text-muted-foreground">{node.engines.length} engine(s)</span>
						</div>

						{#if node.engines.length === 0}
							<p class="text-xs text-muted-foreground/70">no models resident</p>
						{:else}
							<ul class="space-y-1.5">
								{#each node.engines as e (e.base)}
									<li class="flex items-center justify-between gap-2 text-sm">
										<span class="flex items-baseline gap-2 truncate">
											<span class="truncate font-medium text-foreground">{e.base}</span>
											<span class="shrink-0 text-xs text-muted-foreground">
												C {e.max_num_seqs ?? '–'} · headroom {e.headroom ?? '–'}
											</span>
										</span>
										<Button
											variant="ghost"
											size="sm"
											class="h-6 shrink-0 px-2 text-xs"
											disabled={busy !== null}
											onclick={() => act(node.runner_id, 'unload', e.base)}
										>
											{busy === `${node.runner_id}:${e.base}:unload` ? '…' : 'Unload'}
										</Button>
									</li>
									{#if e.loaded_adapters.length > 0}
										<li class="pl-3 text-xs text-muted-foreground">
											adapters: {e.loaded_adapters.map((a) => a.model_id).join(', ')}
										</li>
									{/if}
								{/each}
							</ul>
						{/if}

						<!-- Provisioned to disk, NOT resident — one click to load (no
							 re-download). The runner-local "ready to load" browser. -->
						{#if (node.pulled ?? []).length > 0}
							<ul class="mt-2 space-y-1 border-t border-border/40 pt-2">
								<li class="text-xs font-medium text-muted-foreground/70">ready to load</li>
								{#each node.pulled ?? [] as p (p)}
									<li class="flex items-center justify-between gap-2 text-sm">
										<span class="truncate text-muted-foreground">{p}</span>
										<Button
											variant="ghost"
											size="sm"
											class="h-6 shrink-0 px-2 text-xs"
											disabled={busy !== null}
											onclick={() => act(node.runner_id, 'load', p)}
										>
											{busy === `${node.runner_id}:${p}:load` ? '…' : 'Load'}
										</Button>
									</li>
								{/each}
							</ul>
						{/if}

						<!-- Provision / load a model by id, or browse official catalogs. -->
						<div class="mt-2 flex items-center gap-1.5 border-t border-border/40 pt-2">
							<input
								class="h-7 min-w-0 flex-1 rounded-md border border-border/60 bg-background px-2 text-xs"
								placeholder="model id (e.g. llama3.2:1b)"
								bind:value={loadInputs[node.runner_id]}
							/>
							<Button
								variant="ghost"
								size="sm"
								class="h-7 shrink-0 px-2 text-xs"
								disabled={busy !== null || !loadInputs[node.runner_id]}
								onclick={() => act(node.runner_id, 'pull', loadInputs[node.runner_id] ?? '')}
								title="Provision (download) to disk without loading"
							>
								Pull
							</Button>
							<Button
								variant="outline"
								size="sm"
								class="h-7 shrink-0 px-2 text-xs"
								disabled={busy !== null || !loadInputs[node.runner_id]}
								onclick={() => act(node.runner_id, 'load', loadInputs[node.runner_id] ?? '')}
							>
								Load
							</Button>
						</div>
						<div class="mt-1.5 flex justify-end">
							<Button
								variant="ghost"
								size="sm"
								class="h-6 gap-1 px-2 text-xs text-muted-foreground"
								disabled={busy !== null}
								onclick={() => openBrowser(node.runner_id)}
							>
								<Search class="size-3.5" />
								Browse catalog
							</Button>
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>

	<!-- MODEL SET — operator-curated loaded set -->
	<div class="space-y-2">
		<h3 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
			Curated model set
		</h3>
		{#if models.length === 0}
			<p class="text-xs text-muted-foreground/70">
				No curated models. Add a <code>model_registry</code> resource to approve a model into the pool.
			</p>
		{:else}
			<div class="flex flex-wrap gap-2">
				{#each models as m (m.model_id)}
					<span
						class="inline-flex items-center gap-1.5 rounded-md border border-border/60 bg-card px-2 py-1 text-xs"
						title={m.base ? `LoRA of ${m.base}` : 'base model'}
					>
						<span
							class="size-1.5 rounded-full {m.available
								? 'bg-emerald-500'
								: 'bg-muted-foreground/40'}"
						></span>
						<span class="font-medium text-foreground">{m.model_id}</span>
						<span class={statusTone(String(m.state))}>{m.state}</span>
					</span>
				{/each}
			</div>
		{/if}
	</div>

	<!-- POLICIES — per-model demand/placement (model_policy + model_replicas) -->
	<div class="space-y-2">
		<h3 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
			Placement policies
		</h3>
		{#if policies.length === 0}
			<p class="text-xs text-muted-foreground/70">No model policies.</p>
		{:else}
			<div class="grid gap-2 sm:grid-cols-2">
				{#each policies as p (p.id)}
					{@const r = replicaFor(p.id)}
					<div class="rounded-lg border border-border/60 bg-card p-2.5 text-sm">
						<div class="flex items-center justify-between">
							<span class="font-medium text-foreground">{p.display_name || p.path}</span>
							{#if r}
								<span class="text-xs {statusTone(r.status)}">{r.status}</span>
							{/if}
						</div>
						<div class="mt-0.5 text-xs text-muted-foreground">
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

	<!-- NODE POOLS — engine capacity (node_pool + node_replicas). Node provisioning
		 (Nomad) is deferred; surfaced read-only. -->
	<div class="space-y-2">
		<h3 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
			Node pools
			<span class="ml-1 font-normal normal-case text-muted-foreground/60">
				(engine capacity — provisioning deferred)
			</span>
		</h3>
		{#if pools.length === 0}
			<p class="text-xs text-muted-foreground/70">No node pools.</p>
		{:else}
			<div class="grid gap-2 sm:grid-cols-2">
				{#each pools as p (p.id)}
					{@const r = nodeReplicaFor(p.id)}
					<div class="rounded-lg border border-border/60 bg-card p-2.5 text-sm">
						<div class="flex items-center justify-between">
							<span class="font-medium text-foreground">{p.display_name || p.path}</span>
							{#if r}<span class="text-xs {statusTone(r.status)}">{r.status}</span>{/if}
						</div>
						<div class="mt-0.5 text-xs text-muted-foreground">
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
</section>

<!-- Model browser — opened against a runner; "Provision" pulls onto it. -->
<ModelBrowser
	bind:open={browserOpen}
	runnerLabel={browserRunner ? `runner ${shortId(browserRunner)}` : ''}
	onprovision={onProvision}
/>
