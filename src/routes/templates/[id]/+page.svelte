<script lang="ts">
	import { page } from '$app/state';
	import WorkflowCanvas from '$lib/components/editor/WorkflowCanvas.svelte';
	import NodePropertyPanel from '$lib/components/editor/panels/NodePropertyPanel.svelte';
	import EditorToolbar from '$lib/components/editor/toolbar/EditorToolbar.svelte';
	import { getTemplate, updateTemplate, publishTemplate } from '$lib/api/client';
	import { compileToAIR } from '$lib/compiler/compile';
	import type { Template } from '$lib/types/api';
	import type { WorkflowGraph, WorkflowNodeData, WorkflowNodeType } from '$lib/types/editor';

	const templateId = $derived(page.params.id!);

	let template = $state<Template | null>(null);
	let loading = $state(true);
	let saving = $state(false);
	let error = $state<string | null>(null);
	let selectedNodeId = $state<string | null>(null);
	let airPreview = $state<object | null>(null);

	// Current graph state (kept in sync with canvas)
	let currentGraph = $state<WorkflowGraph>(defaultGraph());

	function defaultGraph(): WorkflowGraph {
		return {
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
					position: { x: 500, y: 200 },
					data: { type: 'end', label: 'End' }
				}
			],
			edges: []
		};
	}

	// Load template from API
	async function load() {
		if (templateId === 'new') {
			// New template mode - use defaults
			template = null;
			currentGraph = defaultGraph();
			loading = false;
			return;
		}

		loading = true;
		error = null;
		try {
			template = await getTemplate(templateId);
			currentGraph = template.graph ?? defaultGraph();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load template';
			// Fallback to default graph for development
			currentGraph = defaultGraph();
		} finally {
			loading = false;
		}
	}

	async function handleSave() {
		if (!template || template.published) return;
		saving = true;
		try {
			template = await updateTemplate(template.id, { graph: currentGraph });
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to save';
		} finally {
			saving = false;
		}
	}

	async function handlePublish() {
		if (!template || template.published) return;
		// Save first, then publish
		try {
			saving = true;
			await updateTemplate(template.id, { graph: currentGraph });
			template = await publishTemplate(template.id);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to publish';
		} finally {
			saving = false;
		}
	}

	function handlePreview() {
		const result = compileToAIR(
			currentGraph,
			template?.name ?? 'Untitled',
			template?.description
		);
		if (result.errors.length > 0) {
			error = result.errors.map((e) => e.message).join('; ');
			return;
		}
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

	$effect(() => {
		load();
	});
</script>

{#if loading}
	<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
		Loading editor...
	</div>
{:else}
	<div class="flex h-full flex-col">
		<EditorToolbar
			templateName={template?.name ?? 'New Workflow'}
			published={template?.published ?? false}
			{saving}
			onsave={handleSave}
			onpublish={handlePublish}
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
				readonly={template?.published ?? false}
				onchange={handleGraphChange}
				onselect={handleNodeSelect}
			/>

			{#if selectedNodeData && !(template?.published)}
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
{/if}
