<script lang="ts">
	import WorkflowCanvas from '$lib/components/editor/WorkflowCanvas.svelte';
	import NodePropertyPanel from '$lib/components/editor/panels/NodePropertyPanel.svelte';
	import EditorToolbar from '$lib/components/editor/toolbar/EditorToolbar.svelte';
	import { compileToAIR } from '$lib/compiler/compile';
	import type { WorkflowGraph, WorkflowNodeData } from '$lib/types/editor';

	let error = $state<string | null>(null);
	let selectedNodeId = $state<string | null>(null);
	let airPreview = $state<object | null>(null);

	let currentGraph = $state<WorkflowGraph>({
		nodes: [
			{
				id: 'node-start',
				type: 'start',
				position: { x: 100, y: 200 },
				data: { type: 'start', label: 'Start' }
			},
			{
				id: 'node-end',
				type: 'end',
				position: { x: 600, y: 200 },
				data: { type: 'end', label: 'End' }
			}
		],
		edges: []
	});

	function handlePreview() {
		const result = compileToAIR(currentGraph, 'Demo Workflow');
		if (result.errors.length > 0) {
			error = result.errors.map((e) => e.message).join('; ');
			airPreview = null;
			return;
		}
		error = null;
		airPreview = result.air as unknown as object;
	}

	function handleGraphChange(graph: WorkflowGraph) {
		currentGraph = graph;
	}

	function handleNodeSelect(nodeId: string | null) {
		selectedNodeId = nodeId;
	}

	function handleNodeDataChange(data: WorkflowNodeData) {
		if (!selectedNodeId) return;
		currentGraph = {
			...currentGraph,
			nodes: currentGraph.nodes.map((n) =>
				n.id === selectedNodeId ? { ...n, data } : n
			)
		};
	}

	const selectedNodeData = $derived(
		selectedNodeId
			? currentGraph.nodes.find((n) => n.id === selectedNodeId)?.data ?? null
			: null
	);
</script>

<div class="flex h-full flex-col">
	<EditorToolbar
		templateName="Demo Workflow"
		published={false}
		saving={false}
		onsave={() => {}}
		onpublish={() => {}}
		onpreview={handlePreview}
	/>

	{#if error}
		<div class="border-b border-amber-200 bg-amber-50 px-4 py-2 text-xs text-amber-800">
			{error}
			<button
				type="button"
				class="ml-2 underline"
				onclick={() => (error = null)}>dismiss</button
			>
		</div>
	{/if}

	<div class="relative flex flex-1 overflow-hidden">
		<WorkflowCanvas
			graph={currentGraph}
			readonly={false}
			onchange={handleGraphChange}
			onselect={handleNodeSelect}
		/>

		{#if selectedNodeData}
			<NodePropertyPanel
				data={selectedNodeData}
				onchange={handleNodeDataChange}
				onclose={() => (selectedNodeId = null)}
			/>
		{/if}
	</div>

	{#if airPreview}
		<div class="border-t border-border bg-muted/50">
			<div class="flex items-center justify-between px-3 py-1.5">
				<span class="text-xs font-medium text-muted-foreground">AIR Preview</span>
				<button
					type="button"
					class="text-xs text-muted-foreground underline"
					onclick={() => (airPreview = null)}>close</button
				>
			</div>
			<pre class="max-h-64 overflow-auto px-3 pb-2 font-mono text-[10px] text-foreground">
{JSON.stringify(airPreview, null, 2)}
			</pre>
		</div>
	{/if}
</div>
