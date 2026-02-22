<script lang="ts">
	import { goto } from '$app/navigation';
	import WorkflowCanvas from '$lib/components/editor/WorkflowCanvas.svelte';
	import NodePropertyPanel from '$lib/components/editor/panels/NodePropertyPanel.svelte';
	import { Sheet, SheetContent, SheetTitle, SheetDescription } from '$lib/components/ui/sheet';
	import { getSheetWidthClass } from '$lib/components/editor/panels/panel-width';
	import { compileGraph, createTemplate, publishTemplate, createInstance } from '$lib/api/client';
	import type { WorkflowGraph, WorkflowNodeData } from '$lib/types/editor';
	import { showcaseGraph } from './showcase-graph';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import Rocket from '@lucide/svelte/icons/rocket';

	let error = $state<string | null>(null);
	let selectedNodeId = $state<string | null>(null);
	let panelExpanded = $state(false);
	let airPreview = $state<object | null>(null);
	let deploying = $state(false);

	let currentGraph = $state<WorkflowGraph>(structuredClone(showcaseGraph));

	async function handlePreview() {
		try {
			airPreview = await compileGraph({ name: 'Invoice Processing Demo', graph: currentGraph });
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Compilation failed';
			airPreview = null;
		}
	}

	async function handleRunInstance() {
		deploying = true;
		error = null;
		try {
			// 1. Create template from demo graph
			const template = await createTemplate({
				name: 'Invoice Processing Demo',
				description: 'Auto-created from demo page showcase workflow',
				graph: currentGraph,
				author_id: '00000000-0000-0000-0000-000000000000'
			});

			// 2. Publish it
			await publishTemplate(template.id);

			// 3. Create an instance
			const instance = await createInstance({
				template_id: template.id,
				created_by: '00000000-0000-0000-0000-000000000000'
			});

			// 4. Navigate to watch it run
			goto(`/instances/${instance.id}`);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to deploy. Is mekhan-service running?';
		} finally {
			deploying = false;
		}
	}

	function handleGraphChange(graph: WorkflowGraph) {
		currentGraph = graph;
	}

	function handleNodeSelect(nodeId: string | null) {
		selectedNodeId = nodeId;
		panelExpanded = false;
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

	function handleReset() {
		currentGraph = structuredClone(showcaseGraph);
		selectedNodeId = null;
		airPreview = null;
		error = null;
	}

	const selectedNodeData = $derived(
		selectedNodeId
			? currentGraph.nodes.find((n) => n.id === selectedNodeId)?.data ?? null
			: null
	);
</script>

<div class="flex h-full flex-col" data-testid="demo-page">
	<div class="flex h-10 items-center justify-between border-b border-border bg-card px-3" data-testid="demo-toolbar">
		<div class="flex items-center gap-3">
			<span class="text-sm font-medium text-foreground">Invoice Processing Demo</span>
			<Badge class="bg-cyan-100 text-cyan-700" variant="secondary">
				Showcase
			</Badge>
		</div>

		<div class="flex items-center gap-1.5">
			<Button variant="ghost" size="sm" onclick={handleReset}>
				Reset
			</Button>

			<Button variant="ghost" size="sm" data-testid="btn-preview-air" onclick={handlePreview}>
				Preview AIR
			</Button>

			<Button size="sm" data-testid="btn-run-instance" disabled={deploying} onclick={handleRunInstance}>
				<Rocket class="size-3.5" />
				{deploying ? 'Deploying...' : 'Run Instance'}
			</Button>
		</div>
	</div>

	{#if error}
		<div class="border-b border-amber-200 bg-amber-50 px-4 py-2 text-xs text-amber-800">
			{error}
			<Button variant="link" size="sm" class="ml-2 h-auto p-0 text-xs" onclick={() => (error = null)}>dismiss</Button>
		</div>
	{/if}

	<div class="relative flex flex-1 overflow-hidden">
		<WorkflowCanvas
			graph={currentGraph}
			readonly={false}
			onchange={handleGraphChange}
			onselect={handleNodeSelect}
		/>

		{#if selectedNodeData && !panelExpanded}
			<NodePropertyPanel
				data={selectedNodeData}
				onchange={handleNodeDataChange}
				onclose={() => (selectedNodeId = null)}
				onexpand={() => (panelExpanded = true)}
			/>
		{/if}

		{#if panelExpanded && selectedNodeData}
			<Sheet.Root
				open
				onOpenChange={(open) => { if (!open) panelExpanded = false; }}
			>
				<SheetContent class={getSheetWidthClass(selectedNodeData)}>
					<SheetTitle>Node Properties</SheetTitle>
					<SheetDescription>Edit the selected node</SheetDescription>
					<NodePropertyPanel
						data={selectedNodeData}
						expanded
						onchange={handleNodeDataChange}
						onclose={() => (selectedNodeId = null)}
						oncollapse={() => (panelExpanded = false)}
					/>
				</SheetContent>
			</Sheet.Root>
		{/if}
	</div>

	{#if airPreview}
		<div class="border-t border-border bg-muted/50" data-testid="air-preview-panel">
			<div class="flex items-center justify-between px-3 py-1.5">
				<span class="text-xs font-medium text-muted-foreground">AIR Preview</span>
				<Button variant="link" size="sm" class="h-auto p-0 text-xs" onclick={() => (airPreview = null)}>close</Button>
			</div>
			<pre class="max-h-64 overflow-auto px-3 pb-2 font-mono text-[10px] text-foreground">
{JSON.stringify(airPreview, null, 2)}
			</pre>
		</div>
	{/if}
</div>
