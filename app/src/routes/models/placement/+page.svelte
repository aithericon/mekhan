<script lang="ts">
	// POOLS tab — the autoscaler's engine-capacity fleets (docs/31 Loop 1). One
	// read model: `node_pool` resources ⋈ their `node_replicas` reconciliation row
	// (desired/observed nodes + slots + zone + last error). The per-model autoscale
	// policy that used to live here moved onto the Set tab (folded onto the model).
	//
	// Pools are plain typed resources, so create/edit go through the generic
	// resources API: `public_config` carries the NodePoolPolicy fields.
	import { listNodeReplicas, type NodeReplicaRow } from '$lib/api/models';
	import {
		listResources,
		createResource,
		updateResource,
		getResource,
		type ResourceSummary,
		type ResourceDetail
	} from '$lib/api/resources';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Dialog from '$lib/components/ui/dialog';
	import * as Select from '$lib/components/ui/select';
	import Plus from '@lucide/svelte/icons/plus';
	import { toast } from 'svelte-sonner';
	import { statusTone } from '$lib/components/fleet/model-pool';

	let pools = $state<ResourceSummary[]>([]);
	let nodeReplicas = $state<NodeReplicaRow[]>([]);
	let datacenters = $state<ResourceSummary[]>([]);
	let error = $state<string | null>(null);

	async function poll() {
		try {
			const [pl, nr, dc] = await Promise.all([
				listResources({ resource_type: 'node_pool', perPage: 100 }),
				listNodeReplicas(),
				listResources({ resource_type: 'datacenter', perPage: 100 })
			]);
			pools = pl.items;
			nodeReplicas = nr;
			datacenters = dc.items;
			error = null;
		} catch (err) {
			error = err instanceof Error ? err.message : 'Failed to load pools';
		}
	}

	$effect(() => {
		void poll();
		const t = setInterval(() => void poll(), 5000);
		return () => clearInterval(t);
	});

	const nodeReplicaFor = (poolId: string) => nodeReplicas.find((r) => r.pool_resource_id === poolId);

	// ── Create / edit dialog ──────────────────────────────────────────────────
	let editorOpen = $state(false);
	let editingId = $state<string | null>(null); // null ⇒ create
	let saving = $state(false);

	let fPath = $state('');
	let fName = $state('');
	let fDatacenter = $state('');
	let fZone = $state('');
	let fGpuClass = $state('');
	let fMaxSeqs = $state('');
	let fMinNodes = $state('');
	let fMaxNodes = $state('');
	let fCooldown = $state('');
	let fEngineSpec = $state('{}');

	function resetForm() {
		fPath = '';
		fName = '';
		fDatacenter = '';
		fZone = '';
		fGpuClass = '';
		fMaxSeqs = '';
		fMinNodes = '';
		fMaxNodes = '';
		fCooldown = '';
		fEngineSpec = '{}';
	}

	function openCreate() {
		editingId = null;
		resetForm();
		editorOpen = true;
	}

	async function openEdit(p: ResourceSummary) {
		editingId = p.id;
		resetForm();
		fPath = p.path;
		fName = p.display_name ?? '';
		editorOpen = true;
		try {
			const detail: ResourceDetail = await getResource(p.id);
			const cfg = (detail.public_config ?? {}) as Record<string, unknown>;
			fDatacenter = String(cfg.datacenter_resource_id ?? '');
			fZone = String(cfg.residency_zone ?? '');
			fGpuClass = String(cfg.gpu_class ?? '');
			fMaxSeqs = cfg.max_num_seqs != null ? String(cfg.max_num_seqs) : '';
			fMinNodes = cfg.min_nodes != null ? String(cfg.min_nodes) : '';
			fMaxNodes = cfg.max_nodes != null ? String(cfg.max_nodes) : '';
			fCooldown = cfg.cooldown_secs != null ? String(cfg.cooldown_secs) : '';
			fEngineSpec = JSON.stringify(cfg.engine_spec ?? {}, null, 2);
		} catch (err) {
			toast.error(err instanceof Error ? err.message : 'Failed to load pool config');
		}
	}

	async function save() {
		let engineSpec: unknown;
		try {
			engineSpec = JSON.parse(fEngineSpec || '{}');
		} catch {
			toast.error('engine_spec must be valid JSON');
			return;
		}
		const config: Record<string, unknown> = {
			datacenter_resource_id: fDatacenter.trim(),
			residency_zone: fZone.trim(),
			gpu_class: fGpuClass.trim(),
			max_num_seqs: Number(fMaxSeqs || 0),
			min_nodes: Number(fMinNodes || 0),
			max_nodes: Number(fMaxNodes || 0),
			engine_spec: engineSpec
		};
		const cooldown = fCooldown.trim();
		if (cooldown) config.cooldown_secs = Number(cooldown);

		saving = true;
		try {
			if (editingId) {
				await updateResource(editingId, {
					display_name: fName || null,
					config
				});
				toast.success(`Updated ${fPath}`);
			} else {
				await createResource({
					path: fPath.trim(),
					resource_type: 'node_pool',
					display_name: fName || null,
					config,
					workspace_id: null
				});
				toast.success(`Created ${fPath.trim()}`);
			}
			editorOpen = false;
			await poll();
		} catch (err) {
			toast.error(err instanceof Error ? err.message : 'Save failed');
		} finally {
			saving = false;
		}
	}
</script>

<div class="space-y-6" data-testid="models-pools">
	<div class="flex items-baseline gap-3">
		<h2 class="text-base font-semibold tracking-tight text-foreground">Pools</h2>
		<span class="text-sm text-muted-foreground">engine capacity — generic vLLM node fleets</span>
		<Button
			variant="outline"
			size="sm"
			class="ml-auto h-7 shrink-0 gap-1 px-2 text-sm"
			data-testid="new-pool"
			onclick={openCreate}
		>
			<Plus class="size-3.5" />
			New pool
		</Button>
	</div>

	{#if error}
		<div
			class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-2 text-sm text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200"
		>
			{error}
		</div>
	{/if}

	{#if pools.length === 0}
		<p class="text-sm text-muted-foreground/70">
			No node pools. Use <b>New pool</b> to declare engine capacity for the autoscaler to fill.
		</p>
	{:else}
		<div class="grid gap-2 sm:grid-cols-2">
			{#each pools as p (p.id)}
				{@const r = nodeReplicaFor(p.id)}
				<div class="rounded-lg border border-border/60 bg-card p-2.5 text-sm" data-testid="pool-row">
					<div class="flex items-center justify-between gap-2">
						<span class="truncate font-medium text-foreground">{p.display_name || p.path}</span>
						<div class="flex shrink-0 items-center gap-2">
							{#if r}<span class="text-sm {statusTone(r.status)}">{r.status}</span>{/if}
							<Button
								variant="outline"
								size="sm"
								class="h-6 px-2 text-sm"
								data-testid="pool-edit"
								onclick={() => openEdit(p)}
							>
								Edit
							</Button>
						</div>
					</div>
					<div class="mt-0.5 text-sm text-muted-foreground">
						{#if r}
							desired {r.desired_nodes} nodes · observed {r.observed_nodes} · slots {r.observed_slots}
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

<!-- Create / edit pool -->
<Dialog.Root bind:open={editorOpen}>
	<Dialog.Content class="sm:max-w-lg" data-testid="pool-dialog">
		<Dialog.Header>
			<Dialog.Title>{editingId ? 'Edit node pool' : 'New node pool'}</Dialog.Title>
			<Dialog.Description>
				A generic vLLM node fleet the autoscaler provisions and fills. Models pack onto a pool via
				their autoscale policy.
			</Dialog.Description>
		</Dialog.Header>
		<div class="grid max-h-[60vh] grid-cols-2 gap-3 overflow-y-auto py-1">
			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground">Path / name</span>
				<Input
					bind:value={fPath}
					placeholder="gpu_pool_a100"
					disabled={editingId !== null}
					class="text-sm"
					data-testid="pool-path"
				/>
			</label>
			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground">Display name</span>
				<Input bind:value={fName} placeholder="A100 pool" class="text-sm" />
			</label>
			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground">Datacenter</span>
				{#if datacenters.length === 0}
					<Input
						bind:value={fDatacenter}
						placeholder="dc_nomad (no datacenter resources found)"
						class="text-sm"
					/>
				{:else}
					<Select.Root
						type="single"
						value={fDatacenter}
						onValueChange={(v) => (fDatacenter = v ?? '')}
					>
						<Select.Trigger class="w-full text-sm" data-testid="pool-datacenter">
							{fDatacenter || '— select a datacenter —'}
						</Select.Trigger>
						<Select.Content>
							{#each datacenters as d (d.id)}
								<Select.Item value={d.path} label={d.display_name || d.path} />
							{/each}
						</Select.Content>
					</Select.Root>
				{/if}
			</label>
			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground">Residency zone</span>
				<Input bind:value={fZone} placeholder="eu-central" class="text-sm" />
			</label>
			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground">GPU class</span>
				<Input bind:value={fGpuClass} placeholder="a100-80gb" class="text-sm" />
			</label>
			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground">Max num seqs (C)</span>
				<Input type="number" min="0" bind:value={fMaxSeqs} placeholder="256" class="text-sm" />
			</label>
			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground">Min nodes</span>
				<Input type="number" min="0" bind:value={fMinNodes} placeholder="0" class="text-sm" />
			</label>
			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground">Max nodes</span>
				<Input type="number" min="0" bind:value={fMaxNodes} placeholder="4" class="text-sm" />
			</label>
			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground"
					>Cooldown <span class="text-muted-foreground/60">(secs, optional)</span></span
				>
				<Input type="number" min="0" bind:value={fCooldown} placeholder="60" class="text-sm" />
			</label>
			<label class="col-span-2 block space-y-1">
				<span class="text-sm text-muted-foreground">Engine spec (JSON)</span>
				<textarea
					bind:value={fEngineSpec}
					rows="5"
					class="w-full rounded-md border border-input bg-background px-2 py-1.5 font-mono text-sm"
					data-testid="pool-engine-spec"
				></textarea>
			</label>
		</div>
		<Dialog.Footer>
			<Button variant="ghost" size="sm" class="text-sm" onclick={() => (editorOpen = false)}
				>Cancel</Button
			>
			<Button
				size="sm"
				class="text-sm"
				disabled={saving || (!editingId && !fPath.trim())}
				data-testid="pool-save"
				onclick={save}
			>
				{saving ? 'Saving…' : editingId ? 'Save' : 'Create'}
			</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
