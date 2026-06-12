<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { onDestroy } from 'svelte';
	import WorkflowCanvas from '$lib/components/editor/WorkflowCanvas.svelte';
	import EditorToolbar from '$lib/components/editor/toolbar/EditorToolbar.svelte';
	import { templateFamilyId } from '$lib/components/editor/toolbar/runs-menu';
	import CreateInstanceDialog from '$lib/components/instances/CreateInstanceDialog.svelte';
	import TestsPanel from '$lib/components/templates/TestsPanel.svelte';
	import TemplateSettingsPanel from '$lib/components/templates/TemplateSettingsPanel.svelte';
	import PublishGateModal from '$lib/components/templates/PublishGateModal.svelte';
	import ShareDialog from '$lib/components/iam/ShareDialog.svelte';
	import { roleAtLeast } from '$lib/api/iam';
	import { PageShell } from '$lib/components/shell';
	import { Sheet, SheetContent, SheetTitle } from '$lib/components/ui/sheet';
	// NodePropertyPanel is lazy-loaded — its static import drags in 17
	// property-section files (every AutomatedStep config panel, HumanTask
	// StepEditor, SubWorkflow, Trigger, etc.) at page-init time. On a cold
	// Vite-dev open that's enough module-eval to keep the main thread busy for
	// ~10s, during which the toolbar shows "Reconnecting" because the Yjs
	// onopen callback can't run. Defer until the user actually selects a node.
	type NodePropertyPanelModule = typeof import(
		'$lib/components/editor/panels/NodePropertyPanel.svelte'
	);
	import {
		getTemplate,
		publishTemplate,
		updateTemplate,
		createNewVersion,
		compileGraph,
		CompileApiError,
		PublishGateError,
		type Template,
		type FailingTestInfo
	} from '$lib/api/client';
	import { compileErrors } from '$lib/editor/compile-errors.svelte';
	import CopyButton from '$lib/components/ui/copy-button/CopyButton.svelte';
	import { buildAssertionScope } from '$lib/editor/assertion-scope';
	import { getSession, releaseSession } from '$lib/yjs/session-store';
	import { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import { setWorkflowDefinitions } from '$lib/editor/workflow-definitions.svelte';
	import { refreshSubworkflowContracts } from '$lib/editor/subworkflow-contracts';
	import type {
		WorkflowNodeData,
		WorkflowNodeType,
		WorkflowEdge
	} from '$lib/types/editor';

	const templateId = $derived(page.params.id!);

	let template = $state<Template | null>(null);
	let ownerName = $state<string | null>(null);
	let loading = $state(true);
	let saving = $state(false);
	let error = $state<string | null>(null);
	let selectedNodeId = $state<string | null>(null);
	let airPreview = $state<object | null>(null);
	let runDialogOpen = $state(false);
	let testsPanelOpen = $state(false);
	let settingsPanelOpen = $state(false);
	let publishGate = $state<FailingTestInfo[] | null>(null);
	let nodePropertyPanelModule = $state<NodePropertyPanelModule | null>(null);
	let shareOpen = $state(false);

	// Object-Admins can manage sharing. `my_effective_role` rides the template
	// DTO (Phase 3) and is re-fetched on a grant change so the Share button +
	// (future) edit gates never show a stale role.
	const canShare = $derived(roleAtLeast(template?.my_effective_role, 'admin'));

	// Yjs session + binding — bound once for the active template; the route
	// remounts on id change, so the initial-value read is intended.
	// svelte-ignore state_referenced_locally
	const session = getSession(templateId);
	const binding = new YjsGraphBinding(session.doc);
	// Local-only undo stack (remote origins untracked — see enableUndo). Safe
	// to enable before the template loads: published mode never routes a
	// mutation through the binding, so the stack just stays empty.
	binding.enableUndo();

	// Load template metadata from API
	async function load() {
		if (templateId === 'new') {
			template = null;
			setWorkflowDefinitions(null);
			loading = false;
			return;
		}

		loading = true;
		error = null;
		try {
			template = await getTemplate(templateId);
			// Stash the workflow `definitions` for the client-side derived-port
			// twin to resolve `$ref` response_formats (absent from the Yjs doc).
			setWorkflowDefinitions(
				(template?.graph as { definitions?: Record<string, unknown> } | undefined)?.definitions ??
					null
			);
			// Private sub-workflows carry an owner; resolve its name for the
			// breadcrumb back to the parent workflow.
			ownerName = null;
			if (template?.owner_template_id) {
				try {
					ownerName = (await getTemplate(template.owner_template_id)).name;
				} catch {
					ownerName = null;
				}
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load template';
		} finally {
			loading = false;
		}
	}

	// "Publish & Run" handoff. Plain (non-rune) on purpose — nothing renders
	// off it; it only routes publish success into opening the run dialog. It
	// survives the publish-gate modal so Force/Retry from the gate still
	// completes the run; closing the gate or any hard failure aborts it.
	let runAfterPublish = false;

	async function handlePublish(force = false) {
		if (!template || template.published) {
			runAfterPublish = false;
			return;
		}
		try {
			saving = true;
			template = await publishTemplate(template.id, force);
			compileErrors.clear();
			publishGate = null;
			if (runAfterPublish) {
				// `template` is the published row now, so the dialog opens in the
				// same state handleRun() would require.
				runAfterPublish = false;
				runDialogOpen = true;
			}
		} catch (e) {
			if (e instanceof PublishGateError) {
				// Keep runAfterPublish: the gate modal's Force publish / Retry
				// re-enter handlePublish and can still finish the handoff.
				publishGate = e.failingTests;
			} else if (e instanceof CompileApiError) {
				runAfterPublish = false;
				compileErrors.set(e.compileErrors);
				error = `${e.message} — ${e.compileErrors.length} issue${e.compileErrors.length === 1 ? '' : 's'} highlighted on the canvas`;
			} else {
				runAfterPublish = false;
				error = e instanceof Error ? e.message : 'Failed to publish';
			}
		} finally {
			saving = false;
		}
	}

	function handlePublishAndRun() {
		runAfterPublish = true;
		void handlePublish(false);
	}

	async function handleNewVersion() {
		if (!template || !template.published || saving) return;
		try {
			saving = true;
			const next = await createNewVersion(template.id);
			// Full document load (not `goto`): the Yjs session + binding are
			// created once at script top from the initial templateId, so a
			// param-only nav would leave the canvas pinned to the published
			// version's doc. See TemplateVersionMenu.select for the same reason.
			window.location.assign(`/templates/${next.id}`);
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
		// Editor-launched runs land on the instance's Workflow (graph) tab —
		// the view that mirrors the canvas the author just left. Other entry
		// points (instances list, rerun) keep the default Process tab via the
		// bare `/instances/{id}` redirect.
		goto(`/instances/${instanceId}/workflow`);
	}

	// Shared by the toolbar's inline rename and the settings-sheet Name field.
	// Optimistic; sets the page banner (for the toolbar) and rethrows so the
	// settings panel can also surface it inline (its sheet covers the banner).
	async function handleRename(name: string) {
		if (!template) return;
		const prev = template;
		template = { ...template, name }; // optimistic
		try {
			template = await updateTemplate(templateId, { name });
		} catch (e) {
			template = prev;
			error = e instanceof Error ? e.message : 'Rename failed';
			throw e;
		}
	}

	// Persist a description edit from the settings panel. Optimistic; rethrows
	// so the panel can surface the failure inline (its sheet covers the page
	// error banner).
	async function handleDescriptionChange(description: string) {
		if (!template) return;
		const prev = template;
		template = { ...template, description }; // optimistic
		try {
			template = await updateTemplate(templateId, { description });
		} catch (e) {
			template = prev;
			throw e;
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

	function handleReparentNodes(
		changes: Array<{ id: string; parentId: string | null; position?: { x: number; y: number } }>
	) {
		for (const { id, parentId, position } of changes) {
			binding.setNodeParent(id, parentId, position);
		}
	}

	function handleResizeNodes(
		changes: Array<{
			id: string;
			width: number;
			height: number;
			position?: { x: number; y: number };
		}>
	) {
		for (const { id, width, height, position } of changes) {
			binding.resizeNode(id, { width, height, position });
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

	function handleUpdateEdge(edgeId: string, patch: { join?: 'gather' | null }) {
		// Sparse patch — only act on keys that are actually present.
		if ('join' in patch) {
			binding.updateEdgeJoin(edgeId, patch.join ?? null);
		}
	}

	function handleNodeSelect(nodeId: string | null) {
		selectedNodeId = nodeId;
	}

	// Cmd/Ctrl+Z → undo, Shift+Cmd/Ctrl+Z or Ctrl+Y → redo. Skipped when the
	// keystroke targets a text field (input/textarea/contenteditable — incl.
	// CodeMirror) so native text-editing undo keeps working, and on published
	// templates (read-only: nothing to undo, and the binding never mutates).
	function isTextEditingTarget(t: EventTarget | null): boolean {
		return (
			t instanceof HTMLInputElement ||
			t instanceof HTMLTextAreaElement ||
			t instanceof HTMLSelectElement ||
			(t instanceof HTMLElement && t.isContentEditable)
		);
	}

	function handleUndoKeydown(e: KeyboardEvent) {
		if (template?.published) return;
		if (!e.metaKey && !e.ctrlKey) return;
		const key = e.key.toLowerCase();
		if (key !== 'z' && key !== 'y') return;
		if (isTextEditingTarget(e.target)) return;
		e.preventDefault();
		if (key === 'y' || e.shiftKey) {
			binding.redo();
		} else {
			binding.undo();
		}
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

	const humanTaskSlugs = $derived(
		binding.graph.nodes
			.filter((n) => (n.data as { type?: string } | null)?.type === 'human_task')
			.map((n) => (n.slug && n.slug.trim() !== '' ? n.slug : n.id))
	);

	const assertionScope = $derived(buildAssertionScope(binding.graph));

	$effect(() => {
		load();
	});

	// Once the Yjs graph has synced, backfill every SubWorkflow node's I/O
	// contract straight from the compiler's resolver (the same `/io-contract`
	// the property panel uses), so the canvas advertises what each sub-workflow
	// consumes/returns without the author opening its panel. Reading
	// `nodes.length` is the sync gate; the plain (non-rune) `contractsRefreshed`
	// flag makes this run exactly once per loaded template — the patch writes
	// back through Yjs but is idempotent (portsEqual), so it can't loop.
	let contractsRefreshed = false;
	$effect(() => {
		if (binding.graph.nodes.length === 0 || contractsRefreshed) return;
		contractsRefreshed = true;
		// The contract backfill writes through the binding (null origin), which
		// would otherwise seed the undo stack with an invisible bookkeeping
		// patch — drop the history once it settles so Cmd+Z starts at the
		// user's first real edit.
		void refreshSubworkflowContracts(binding).finally(() => binding.clearUndoHistory());
	});

	$effect(() => {
		if (selectedNodeId && !nodePropertyPanelModule) {
			import('$lib/components/editor/panels/NodePropertyPanel.svelte').then((m) => {
				nodePropertyPanelModule = m as NodePropertyPanelModule;
			});
		}
	});

	onDestroy(() => {
		binding.destroy();
		releaseSession(templateId);
	});
</script>

<svelte:head>
	<title>{template?.name ?? 'Editor'} | Mekhan</title>
</svelte:head>

<svelte:window onkeydown={handleUndoKeydown} />

<PageShell width="bleed" testid="template-editor-page">
	{#if loading}
		<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
			Loading editor...
		</div>
	{:else}
		<div class="flex h-full flex-col">
			<EditorToolbar
				templateName={template?.name ?? 'New Workflow'}
				ownerId={template?.owner_template_id ?? undefined}
				{ownerName}
				published={template?.published ?? false}
				{saving}
				{templateId}
				version={template?.version}
				runsFamilyId={template ? templateFamilyId(template) : undefined}
				awareness={session.awareness}
				provider={session.provider}
				onpublish={() => handlePublish(false)}
				onpublishrun={template && !template.published ? handlePublishAndRun : undefined}
				onpreview={handlePreview}
				onnewversion={handleNewVersion}
				onrun={handleRun}
				ontests={() => (testsPanelOpen = true)}
				onsettings={template ? () => (settingsPanelOpen = true) : undefined}
				onshare={template && canShare ? () => (shareOpen = true) : undefined}
				onrename={handleRename}
				onundo={() => binding.undo()}
				onredo={() => binding.redo()}
				canUndo={binding.canUndo}
				canRedo={binding.canRedo}
			/>

			{#if error}
				<div class="flex items-center gap-2 border-b border-amber-200 bg-amber-50 px-4 py-2 text-sm text-amber-800">
					<span class="flex-1">{error}</span>
					<CopyButton
						getText={() =>
							compileErrors.errors.length > 0
								? `${error}\n\n${JSON.stringify(compileErrors.errors, null, 2)}`
								: (error ?? '')}
						title="Copy error (with compile diagnostics) as JSON"
						class="text-amber-700 hover:text-amber-900"
					/>
					<button
						type="button"
						class="underline"
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
					onReparentNodes={handleReparentNodes}
					onResizeNodes={handleResizeNodes}
					onAddEdge={handleAddEdge}
					onRemoveEdges={handleRemoveEdges}
					onUpdateEdge={handleUpdateEdge}
				/>

				{#if selectedNodeData && nodePropertyPanelModule}
					{@const NodePropertyPanel = nodePropertyPanelModule.default}
					<NodePropertyPanel
						data={selectedNodeData}
						readonly={template?.published ?? false}
						onchange={handleNodeDataChange}
						onclose={() => (selectedNodeId = null)}
						ondelete={handleDeleteSelectedNode}
						{binding}
						nodeId={selectedNodeId ?? undefined}
						{templateId}
						workspaceId={template?.workspace_id}
						onselectnode={handleNodeSelect}
					/>
				{/if}
			</div>

			{#if airPreview}
				<div class="border-t border-border bg-muted/50" data-testid="air-preview-panel">
					<div class="flex items-center justify-between px-3 py-1.5">
						<span class="text-sm font-medium text-muted-foreground">AIR Preview</span>
						<button
							type="button"
							class="text-sm text-muted-foreground underline"
							onclick={() => (airPreview = null)}>close</button
						>
					</div>
					<pre class="max-h-64 overflow-auto px-3 pb-2 font-mono text-sm text-foreground">
{JSON.stringify(airPreview, null, 2)}
					</pre>
				</div>
			{/if}
		</div>
	{/if}
</PageShell>

<Sheet.Root open={testsPanelOpen} onOpenChange={(o: boolean) => (testsPanelOpen = o)}>
	<SheetContent class="w-full max-w-md p-0 sm:max-w-md">
		<SheetTitle class="sr-only">Tests</SheetTitle>
		{#if template}
			<TestsPanel templateId={template.id} {humanTaskSlugs} {assertionScope} />
		{/if}
	</SheetContent>
</Sheet.Root>

<Sheet.Root open={settingsPanelOpen} onOpenChange={(o: boolean) => (settingsPanelOpen = o)}>
	<SheetContent class="w-full max-w-md p-0 sm:max-w-md">
		<SheetTitle class="sr-only">Template settings</SheetTitle>
		{#if template}
			<TemplateSettingsPanel
				{template}
				onrename={handleRename}
				ondescriptionchange={handleDescriptionChange}
			/>
		{/if}
	</SheetContent>
</Sheet.Root>

<PublishGateModal
	open={publishGate !== null}
	failingTests={publishGate ?? []}
	onclose={() => {
		publishGate = null;
		// Abandoning the gate abandons a pending Publish & Run handoff too.
		runAfterPublish = false;
	}}
	onforce={async () => {
		publishGate = null;
		await handlePublish(true);
	}}
	onretry={async () => {
		publishGate = null;
		await handlePublish(false);
	}}
/>

<CreateInstanceDialog
	bind:open={runDialogOpen}
	templateId={template?.id ?? null}
	oncreated={onInstanceCreated}
/>

{#if template}
	<ShareDialog
		bind:open={shareOpen}
		objectType="template"
		objectId={template.id}
		objectName={template.name}
		myEffectiveRole={template.my_effective_role}
		onChanged={load}
	/>
{/if}
