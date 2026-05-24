<script lang="ts">
	import {
		getTemplate,
		listStepExecutions,
		type StepExecution,
		type Template,
		type WorkflowInstance,
		type WorkflowNode
	} from '$lib/api/client';
	import type { WorkflowGraph } from '$lib/api/client';
	import WorkflowCanvas from '$lib/components/editor/WorkflowCanvas.svelte';
	import StepDetailDrawer from './StepDetailDrawer.svelte';
	import { provideNodeRuntime } from './runtime-context';

	type Props = {
		instance: WorkflowInstance;
	};

	let { instance }: Props = $props();

	let template = $state<Template | null>(null);
	let executions = $state<StepExecution[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let selectedNodeId = $state<string | null>(null);
	let drawerStep = $state<StepExecution | null>(null);
	let drawerNode = $state<WorkflowNode | null>(null);
	let drawerIterations = $state<StepExecution[]>([]);
	let drawerOpen = $state(false);

	const isTerminal = $derived(
		instance.status === 'completed' ||
			instance.status === 'failed' ||
			instance.status === 'cancelled'
	);

	// `node_id → executions[]` so Loop body nodes can carry every iteration's
	// row. Ordered by iteration_index for deterministic "latest" lookups.
	const executionsByNode = $derived.by(() => {
		const map = new Map<string, StepExecution[]>();
		for (const e of executions) {
			const list = map.get(e.node_id) ?? [];
			list.push(e);
			map.set(e.node_id, list);
		}
		for (const list of map.values()) {
			list.sort((a, b) => a.iteration_index - b.iteration_index);
		}
		return map;
	});

	// Provide the lookup to every descendant node component via Svelte
	// context. `WorkflowNodeCard` (composed by every standard node) and
	// `LoopNode` read it through `useNodeRuntime` and render a status badge.
	provideNodeRuntime((nodeId: string) => executionsByNode.get(nodeId) ?? []);

	const graph = $derived<WorkflowGraph | null>(
		template?.graph ? (template.graph as WorkflowGraph) : null
	);

	// `node_id → WorkflowNode` lookup so the drawer can show the node's
	// label/description and its raw config payload.
	const nodesById = $derived.by(() => {
		const map = new Map<string, WorkflowNode>();
		if (!graph) return map;
		for (const n of graph.nodes) map.set(n.id, n);
		return map;
	});

	async function loadTemplate() {
		try {
			template = await getTemplate(instance.template_id);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load template';
		}
	}

	async function refreshExecutions() {
		try {
			executions = await listStepExecutions(instance.id);
		} catch (e) {
			// Keep the canvas visible even if the projection fetch transiently
			// fails — the badge just won't render.
			error = e instanceof Error ? e.message : String(e);
		}
	}

	$effect(() => {
		void instance.id;
		loading = true;
		error = null;
		(async () => {
			await Promise.all([loadTemplate(), refreshExecutions()]);
			loading = false;
		})();
	});

	$effect(() => {
		if (isTerminal) return;
		const t = setInterval(refreshExecutions, 2000);
		return () => clearInterval(t);
	});

	function handleSelect(nodeId: string | null) {
		selectedNodeId = nodeId;
		if (!nodeId) {
			drawerOpen = false;
			return;
		}
		const list = executionsByNode.get(nodeId) ?? [];
		const node = nodesById.get(nodeId) ?? null;
		drawerNode = node;
		drawerIterations = list;
		if (list.length === 0) {
			// Step hasn't fired yet — still open the drawer so the user gets
			// the node metadata + a "View config" button, just no runtime data.
			drawerStep = null;
			drawerOpen = !!node;
			return;
		}
		drawerStep = list[list.length - 1];
		drawerOpen = true;
	}

	function selectIteration(iterationIndex: number) {
		const list = drawerIterations;
		const found = list.find((e) => e.iteration_index === iterationIndex);
		if (found) drawerStep = found;
	}

	function closeDrawer() {
		drawerOpen = false;
	}
</script>

<div class="relative h-full w-full">
	{#if loading && !graph}
		<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
			Loading workflow…
		</div>
	{:else if error && !graph}
		<div class="m-6 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{:else if graph}
		<WorkflowCanvas {graph} readonly onselect={handleSelect} />
	{:else}
		<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
			Template not available.
		</div>
	{/if}
</div>

<StepDetailDrawer
	step={drawerStep}
	node={drawerNode}
	iterations={drawerIterations}
	open={drawerOpen}
	onClose={closeDrawer}
	onSelectIteration={selectIteration}
/>
