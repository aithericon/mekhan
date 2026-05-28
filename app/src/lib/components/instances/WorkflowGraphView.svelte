<script lang="ts">
	import {
		getTemplate,
		listStepExecutions,
		listInstanceChildren,
		type InstanceChild,
		type StepExecution,
		type Template,
		type WorkflowInstance,
		type WorkflowNode
	} from '$lib/api/client';
	import type { WorkflowGraph } from '$lib/api/client';
	import { parseInterfaceRegistry, type InterfaceRegistry } from '$lib/types/node-interface';
	import WorkflowCanvas from '$lib/components/editor/WorkflowCanvas.svelte';
	import StepDetailDrawer from './StepDetailDrawer.svelte';
	import { provideNodeRuntime } from './runtime-context';
	import { groupChildrenByNode } from './subworkflow-children';

	type Props = {
		instance: WorkflowInstance;
	};

	let { instance }: Props = $props();

	let template = $state<Template | null>(null);
	let executions = $state<StepExecution[]>([]);
	let children = $state<InstanceChild[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let drawerStep = $state<StepExecution | null>(null);
	let drawerNode = $state<WorkflowNode | null>(null);
	let drawerNodeInterface = $state<import('$lib/types/node-interface').NodeInterface | null>(null);
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

	// `parent_node_id → child instances[]` (ordered by spawn/iteration order)
	// so the drawer can offer an "Enter sub-workflow" drill-in per SubWorkflow
	// node. A SubWorkflow inside a Loop/Map spawns one child per iteration.
	const childrenByNode = $derived(groupChildrenByNode(children));

	// Children for the node the drawer is currently showing.
	const drawerChildren = $derived(
		drawerNode ? (childrenByNode.get(drawerNode.id) ?? []) : []
	);

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

	// Compiler-derived per-node interface (entry/data_port/owned_*/borrowed_paths).
	// `template.interface_json` is typed as `unknown` over the wire; coerce
	// once and look up by node id when opening the drawer.
	const interfaceRegistry = $derived<InterfaceRegistry>(
		parseInterfaceRegistry(template?.interface_json)
	);

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

	async function refreshChildren() {
		try {
			children = await listInstanceChildren(instance.id);
		} catch {
			// Non-fatal: drill-in just won't appear this tick.
		}
	}

	$effect(() => {
		void instance.id;
		loading = true;
		error = null;
		// Drilling parent→child is a param-only navigation within the same
		// /instances/[id] route, so this component is reused (not remounted)
		// and the drawer state survives. Reset it here so a leftover drawer
		// from the parent run (pointing at its SubWorkflow step) doesn't linger
		// over the child's graph.
		drawerOpen = false;
		drawerStep = null;
		drawerNode = null;
		drawerIterations = [];
		(async () => {
			await Promise.all([loadTemplate(), refreshExecutions(), refreshChildren()]);
			loading = false;
		})();
	});

	$effect(() => {
		if (isTerminal) return;
		const t = setInterval(() => {
			void refreshExecutions();
			void refreshChildren();
		}, 2000);
		return () => clearInterval(t);
	});

	function openDrawerFor(nodeId: string) {
		const list = executionsByNode.get(nodeId) ?? [];
		const node = nodesById.get(nodeId) ?? null;
		drawerNode = node;
		drawerNodeInterface = interfaceRegistry[nodeId] ?? null;
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
		<!-- onNodeClick / onPaneClick (rather than onselect) drives the
		     drawer: those fire only on real user pointer events, so the
		     drawer never reopens on its own when xyflow re-emits selection
		     after a `store.nodes` reassignment from polled runtime data. -->
		<WorkflowCanvas
			{graph}
			readonly
			onNodeClick={openDrawerFor}
			onPaneClick={closeDrawer}
		/>
	{:else}
		<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
			Template not available.
		</div>
	{/if}
</div>

<StepDetailDrawer
	step={drawerStep}
	node={drawerNode}
	nodeInterface={drawerNodeInterface}
	iterations={drawerIterations}
	instanceId={instance.id}
	childInstances={drawerChildren}
	open={drawerOpen}
	onClose={closeDrawer}
	onSelectIteration={selectIteration}
/>
