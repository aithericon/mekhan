<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { onDestroy } from 'svelte';
	import WorkflowCanvas from '$lib/components/editor/WorkflowCanvas.svelte';
	import NodePropertyPanel from '$lib/components/editor/panels/NodePropertyPanel.svelte';
	import { Sheet, SheetContent, SheetTitle, SheetDescription } from '$lib/components/ui/sheet';
	import { getSheetWidthClass } from '$lib/components/editor/panels/panel-width';
	import EditorToolbar from '$lib/components/editor/toolbar/EditorToolbar.svelte';
	import CreateInstanceDialog from '$lib/components/instances/CreateInstanceDialog.svelte';
	import {
		getTemplate,
		publishTemplate,
		updateTemplate,
		createNewVersion,
		compileGraph,
		CompileApiError,
		type Template
	} from '$lib/api/client';
	import { compileErrors } from '$lib/editor/compile-errors.svelte';
	import { getSession, releaseSession } from '$lib/yjs/session-store';
	import { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type {
		WorkflowNodeData,
		WorkflowNodeType,
		WorkflowEdge
	} from '$lib/types/editor';

	const templateId = $derived(page.params.id!);

	let template = $state<Template | null>(null);
	let loading = $state(true);
	let saving = $state(false);
	let error = $state<string | null>(null);
	let selectedNodeId = $state<string | null>(null);
	let panelExpanded = $state(false);
	let airPreview = $state<object | null>(null);
	let runDialogOpen = $state(false);

	// Yjs session + binding
	const session = getSession(templateId);
	const binding = new YjsGraphBinding(session.doc);

	// Load template metadata from API
	async function load() {
		if (templateId === 'new') {
			template = null;
			loading = false;
			return;
		}

		loading = true;
		error = null;
		try {
			template = await getTemplate(templateId);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load template';
		} finally {
			loading = false;
		}
	}

	async function handlePublish() {
		if (!template || template.published) return;
		try {
			saving = true;
			template = await publishTemplate(template.id);
			compileErrors.clear();
		} catch (e) {
			if (e instanceof CompileApiError) {
				compileErrors.set(e.compileErrors);
				error = `${e.message} — ${e.compileErrors.length} issue${e.compileErrors.length === 1 ? '' : 's'} highlighted on the canvas`;
			} else {
				error = e instanceof Error ? e.message : 'Failed to publish';
			}
		} finally {
			saving = false;
		}
	}

	async function handleNewVersion() {
		if (!template || !template.published || saving) return;
		try {
			saving = true;
			const next = await createNewVersion(template.id);
			goto(`/templates/${next.id}`);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to create new version';
			saving = false;
		}
	}

	function handleRun() {
		if (!template?.published) return;
		runDialogOpen = true;
	}

	function onInstanceCreated(instanceId: string) {
		runDialogOpen = false;
		goto(`/instances/${instanceId}`);
	}

	async function handleRename(name: string) {
		if (!template) return;
		const prev = template;
		template = { ...template, name }; // optimistic
		try {
			template = await updateTemplate(templateId, { name });
		} catch (e) {
			template = prev;
			error = e instanceof Error ? e.message : 'Rename failed';
		}
	}

	async function handlePreview() {
		try {
			// Snapshot per-node files so the preview AIR shows the same staging
			// shape that publish would emit (inline as Raw vs. S3 StoragePath).
			const files: Record<string, Record<string, string>> = {};
			for (const node of binding.graph.nodes) {
				const nodeFiles = binding.getNodeFiles(node.id);
				if (nodeFiles.size === 0) continue;
				const entries: Record<string, string> = {};
				for (const [name, ytext] of nodeFiles) {
					entries[name] = ytext.toString();
				}
				files[node.id] = entries;
			}
			airPreview = await compileGraph({
				name: template?.name ?? 'Untitled',
				description: template?.description,
				graph: binding.graph,
				files
			});
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Compilation failed';
			airPreview = null;
		}
	}

	// Granular canvas callbacks → Yjs binding
	function handleAddNode(
		id: string,
		type: WorkflowNodeType,
		position: { x: number; y: number },
		data: WorkflowNodeData,
		opts?: { parentId?: string; width?: number; height?: number }
	) {
		binding.addNode(id, type, position, data, opts);
	}

	function handleRemoveNodes(ids: string[]) {
		for (const id of ids) {
			binding.removeNode(id);
		}
	}

	function handleMoveNodes(moves: Array<{ id: string; position: { x: number; y: number } }>) {
		for (const { id, position } of moves) {
			binding.updateNodePosition(id, position);
		}
	}

	function handleAddEdge(edge: WorkflowEdge) {
		binding.addEdge(edge);
	}

	function handleRemoveEdges(ids: string[]) {
		for (const id of ids) {
			binding.removeEdge(id);
		}
	}

	function handleNodeSelect(nodeId: string | null) {
		selectedNodeId = nodeId;
		panelExpanded = false;
	}

	function handleDeleteSelectedNode() {
		if (!selectedNodeId) return;
		// Mirror WorkflowCanvas.handleDelete cascade: a scope node also removes
		// its children. binding.removeNode already cascades connected edges.
		const idsToDelete = new Set<string>([selectedNodeId]);
		for (const n of binding.graph.nodes) {
			if (n.parentId && idsToDelete.has(n.parentId)) {
				idsToDelete.add(n.id);
			}
		}
		for (const id of idsToDelete) {
			binding.removeNode(id);
		}
		selectedNodeId = null;
		panelExpanded = false;
	}

	function handleNodeDataChange(data: WorkflowNodeData) {
		if (!selectedNodeId) return;
		binding.updateNodeData(selectedNodeId, data);
	}

	const selectedNodeData = $derived(
		selectedNodeId
			? binding.graph.nodes.find((n) => n.id === selectedNodeId)?.data ?? null
			: null
	);

	$effect(() => {
		load();
	});

	onDestroy(() => {
		binding.destroy();
		releaseSession(templateId);
	});
</script>

{#if loading}
	<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
		Loading editor...
	</div>
{:else}
	<div class="flex h-full flex-col" data-testid="template-editor-page">
		<EditorToolbar
			templateName={template?.name ?? 'New Workflow'}
			published={template?.published ?? false}
			{saving}
			{templateId}
			awareness={session.awareness}
			provider={session.provider}
			onpublish={handlePublish}
			onpreview={handlePreview}
			onnewversion={handleNewVersion}
			onrun={handleRun}
			onrename={handleRename}
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
				graph={binding.graph}
				readonly={template?.published ?? false}
				onselect={handleNodeSelect}
				onAddNode={handleAddNode}
				onRemoveNodes={handleRemoveNodes}
				onMoveNodes={handleMoveNodes}
				onAddEdge={handleAddEdge}
				onRemoveEdges={handleRemoveEdges}
			/>

			{#if selectedNodeData && !panelExpanded}
				<NodePropertyPanel
					data={selectedNodeData}
					readonly={template?.published ?? false}
					onchange={handleNodeDataChange}
					onclose={() => (selectedNodeId = null)}
					onexpand={() => (panelExpanded = true)}
					ondelete={handleDeleteSelectedNode}
					{binding}
					nodeId={selectedNodeId ?? undefined}
					{templateId}
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
							readonly={template?.published ?? false}
							expanded
							onchange={handleNodeDataChange}
							onclose={() => (selectedNodeId = null)}
							oncollapse={() => (panelExpanded = false)}
							ondelete={handleDeleteSelectedNode}
							{binding}
							nodeId={selectedNodeId ?? undefined}
							{templateId}
						/>
					</SheetContent>
				</Sheet.Root>
			{/if}
		</div>

		{#if airPreview}
			<div class="border-t border-border bg-muted/50" data-testid="air-preview-panel">
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

<CreateInstanceDialog
	bind:open={runDialogOpen}
	templateId={template?.id ?? null}
	oncreated={onInstanceCreated}
/>
