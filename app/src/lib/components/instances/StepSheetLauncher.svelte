<!--
  StepSheetLauncher — a small button that opens the SAME StepDetailDrawer the
  Steps tab uses, for the step that produced a given artifact.

  Resolving artifact → step: the artifact's executor job id (execution_id) is a
  per-element/fanout job that isn't itself a step row, so we walk the server
  provenance (`getProvenanceFromArtifact`) back to the producing place — its
  node-id prefix (`simulate/artifact_log` → `simulate`) IS the step. We then
  build the drawer exactly as StepsView does (listStepExecutions + getTemplate),
  picking the iteration whose completion is closest to the artifact's timestamp.

  Self-contained + lazy: nothing is fetched until the button is clicked; the
  per-instance steps+template fetch is memoized across launchers.
-->
<script module lang="ts">
	import {
		getTemplate,
		listStepExecutions,
		type StepExecution,
		type WorkflowGraph,
		type WorkflowNode
	} from '$lib/api/client';
	import { parseInterfaceRegistry, type NodeInterface } from '$lib/types/node-interface';

	type Resolved = {
		stepsByNode: Map<string, StepExecution[]>;
		nodesById: Map<string, WorkflowNode>;
		interfaceByNode: Record<string, NodeInterface>;
	};

	// Per-instance memo of (steps + template-derived maps), SHARED across every
	// launcher on the page so repeated opens don't refetch.
	const instanceCache = new Map<string, Promise<Resolved>>();

	function loadInstance(instanceId: string, templateId: string): Promise<Resolved> {
		let p = instanceCache.get(instanceId);
		if (!p) {
			p = (async () => {
				const [steps, template] = await Promise.all([
					listStepExecutions(instanceId),
					getTemplate(templateId)
				]);
				const stepsByNode = new Map<string, StepExecution[]>();
				for (const s of steps) {
					const list = stepsByNode.get(s.node_id) ?? [];
					list.push(s);
					stepsByNode.set(s.node_id, list);
				}
				for (const list of stepsByNode.values())
					list.sort((a, b) => a.iteration_index - b.iteration_index);
				const nodesById = new Map<string, WorkflowNode>();
				const graph = (template?.graph ?? null) as WorkflowGraph | null;
				if (graph) for (const n of graph.nodes) nodesById.set(n.id, n);
				const interfaceByNode = parseInterfaceRegistry(template?.interface_json);
				return { stepsByNode, nodesById, interfaceByNode };
			})();
			instanceCache.set(instanceId, p);
		}
		return p;
	}
</script>

<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import Footprints from '@lucide/svelte/icons/footprints';
	import LoaderCircle from '@lucide/svelte/icons/loader-circle';
	import { getProvenanceFromArtifact } from '$lib/api/client';
	import StepDetailDrawer from './StepDetailDrawer.svelte';

	let {
		instanceId,
		templateId,
		executionId,
		artifactId,
		createdAt = '',
		variant = 'icon'
	}: {
		instanceId: string;
		templateId: string;
		executionId: string;
		/** Logical catalogue id (`artifact_id ?? id`). */
		artifactId: string;
		createdAt?: string;
		variant?: 'icon' | 'inline';
	} = $props();

	function producingNodeId(places: { place_id?: string | null; effect_handler?: string | null }[]): string | null {
		const reg = places.find((n) => n.effect_handler === 'catalogue_register' && n.place_id);
		const pick = reg ?? places.find((n) => n.place_id);
		const pid = pick?.place_id ?? null;
		if (!pid) return null;
		return pid.includes('/') ? pid.split('/')[0] : pid;
	}

	function pickIteration(iters: StepExecution[]): StepExecution | null {
		if (!iters.length) return null;
		if (!createdAt) return iters[iters.length - 1];
		const t = new Date(createdAt).getTime();
		// Closest completed_at (then started_at) to the artifact timestamp.
		let best = iters[iters.length - 1];
		let bestΔ = Infinity;
		for (const s of iters) {
			const ts = s.completed_at ?? s.started_at;
			if (!ts) continue;
			const Δ = Math.abs(new Date(ts).getTime() - t);
			if (Δ < bestΔ) {
				bestΔ = Δ;
				best = s;
			}
		}
		return best;
	}

	let loading = $state(false);
	let error = $state<string | null>(null);
	let open = $state(false);
	let step = $state<StepExecution | null>(null);
	let node = $state<WorkflowNode | null>(null);
	let nodeInterface = $state<NodeInterface | null>(null);
	let iterations = $state<StepExecution[]>([]);

	const canResolve = $derived(!!instanceId && !!templateId && !!executionId && !!artifactId);

	async function openStep() {
		if (!canResolve || loading) return;
		loading = true;
		error = null;
		try {
			const prov = await getProvenanceFromArtifact(executionId, artifactId);
			const nodeId = producingNodeId(prov.nodes ?? []);
			if (!nodeId) throw new Error('Could not resolve the producing step.');
			const { stepsByNode, nodesById, interfaceByNode } = await loadInstance(instanceId, templateId);
			const iters = stepsByNode.get(nodeId) ?? [];
			const picked = pickIteration(iters);
			if (!picked) throw new Error('No step execution found for this artifact.');
			step = picked;
			iterations = iters;
			node = nodesById.get(nodeId) ?? null;
			nodeInterface = interfaceByNode[nodeId] ?? null;
			open = true;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to open step';
		} finally {
			loading = false;
		}
	}

	function selectIteration(iterationIndex: number) {
		const found = iterations.find((e) => e.iteration_index === iterationIndex);
		if (found) step = found;
	}
</script>

{#if canResolve}
	{#if variant === 'inline'}
		<Button variant="outline" size="sm" disabled={loading} onclick={openStep} title="Open the step that produced this">
			{#if loading}<LoaderCircle class="size-4 animate-spin" />{:else}<Footprints class="size-4" />{/if}
			Producing step
		</Button>
	{:else}
		<Button
			variant="ghost"
			size="icon-sm"
			disabled={loading}
			onclick={openStep}
			title="Open the step that produced this"
			aria-label="Open the step that produced this"
		>
			{#if loading}<LoaderCircle class="size-4 animate-spin" />{:else}<Footprints class="size-4" />{/if}
		</Button>
	{/if}
	{#if error}
		<span class="text-xs text-red-500">{error}</span>
	{/if}

	<!-- Mounted only once opened: the drawer is a heavy instance-side component and
	     this launcher lives inside a Yjs editor node view, so we keep it out of the
	     node-view mount path until the user actually asks for it. -->
	{#if open}
		<StepDetailDrawer
			{step}
			{node}
			{nodeInterface}
			{iterations}
			{instanceId}
			open={true}
			onClose={() => (open = false)}
			onSelectIteration={selectIteration}
		/>
	{/if}
{/if}
