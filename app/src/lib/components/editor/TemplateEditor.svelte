<script lang="ts">
	import { goto } from '$app/navigation';
	import { onDestroy, tick } from 'svelte';
	import WorkflowCanvas from '$lib/components/editor/WorkflowCanvas.svelte';
	import EditorToolbar from '$lib/components/editor/toolbar/EditorToolbar.svelte';
	import { templateFamilyId } from '$lib/components/editor/toolbar/runs-menu';
	import CreateInstanceDialog from '$lib/components/instances/CreateInstanceDialog.svelte';
	import PageEditor from '$lib/components/pages/PageEditor.svelte';
	import TestsPanel from '$lib/components/templates/TestsPanel.svelte';
	import TemplateSettingsPanel from '$lib/components/templates/TemplateSettingsPanel.svelte';
	import PublishGateModal from '$lib/components/templates/PublishGateModal.svelte';
	import ShareDialog from '$lib/components/iam/ShareDialog.svelte';
	import { roleAtLeast } from '$lib/api/iam';
	import { PageShell } from '$lib/components/shell';
	import { Sheet, SheetContent, SheetTitle } from '$lib/components/ui/sheet';
	import * as Dialog from '$lib/components/ui/dialog';
	import { Button } from '$lib/components/ui/button';
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
		discardDraft,
		compileGraph,
		ensureAttachedPage,
		CompileApiError,
		PublishGateError,
		type Template,
		type FailingTestInfo,
		type Page
	} from '$lib/api/client';
	import { compileErrors } from '$lib/editor/compile-errors.svelte';
	import CopyButton from '$lib/components/ui/copy-button/CopyButton.svelte';
	import { buildAssertionScope } from '$lib/editor/assertion-scope';
	import { getSession, releaseSession } from '$lib/yjs/session-store';
	import { YjsGraphBinding, type GraphClipboard } from '$lib/yjs/graph-binding.svelte';
	import { getClipboard, setClipboard, nextPasteOffset } from '$lib/editor/graph-clipboard';
	import { setWorkflowDefinitions } from '$lib/editor/workflow-definitions.svelte';
	import { refreshSubworkflowContracts } from '$lib/editor/subworkflow-contracts';
	import type {
		WorkflowNodeData,
		WorkflowNodeType,
		WorkflowEdge
	} from '$lib/types/editor';

	// This component owns ONE template's editing session for its whole
	// lifetime. The route wraps it in `{#key templateId}`, so an in-app nav to
	// another version/template destroys it (binding.destroy + releaseSession →
	// WS close) and mounts a fresh instance — same init path as a cold load.
	let { templateId }: { templateId: string } = $props();

	let template = $state<Template | null>(null);
	let ownerName = $state<string | null>(null);
	let loading = $state(true);
	let saving = $state(false);
	let error = $state<string | null>(null);
	let selectedNodeId = $state<string | null>(null);
	// Full multi-selection mirror (copy/duplicate operate on this); the single
	// `selectedNodeId` keeps driving the property panel.
	let selectedNodeIds = $state<string[]>([]);
	// bind:this seam to the canvas — only the selection setter is exposed.
	let canvasRef = $state<{ setSelectedNodes: (ids: string[]) => void } | null>(null);
	let airPreview = $state<object | null>(null);
	let runDialogOpen = $state(false);
	let testsPanelOpen = $state(false);
	let settingsPanelOpen = $state(false);
	let notesOpen = $state(false);
	// Lazily fetched-or-created on first Notes open. Keyed on the template
	// FAMILY id (chain root), so every version of a workflow shares one page.
	let notesPage = $state<Page | null>(null);
	let notesLoading = $state(false);
	let notesError = $state<string | null>(null);
	let publishGate = $state<FailingTestInfo[] | null>(null);
	let nodePropertyPanelModule = $state<NodePropertyPanelModule | null>(null);
	let shareOpen = $state(false);
	let discardConfirmOpen = $state(false);
	let discarding = $state(false);

	// Object-Admins can manage sharing. `my_effective_role` rides the template
	// DTO (Phase 3) and is re-fetched on a grant change so the Share button +
	// (future) edit gates never show a stale role.
	const canShare = $derived(roleAtLeast(template?.my_effective_role, 'admin'));

	// Yjs session + binding — bound once for this component instance; the
	// `{#key}` wrapper remounts it on id change, so the initial-value read is
	// intended.
	// `compileErrors` is a module-level singleton that used to be wiped by the
	// full-page reload on every version switch. With in-app navigation the
	// `{#key}` remount is the only reset point, so clear it here — otherwise a
	// failed publish on a draft leaks red error rings onto the next version
	// viewed (the fork preserves node ids, so `byNodeId` still matches).
	compileErrors.clear();

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

	async function handlePublish(force = false) {
		if (!template || template.published) return;
		try {
			saving = true;
			template = await publishTemplate(template.id, force);
			compileErrors.clear();
			publishGate = null;
		} catch (e) {
			if (e instanceof PublishGateError) {
				publishGate = e.failingTests;
			} else if (e instanceof CompileApiError) {
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
			// In-app nav: the route's `{#key}` tears this session down and
			// mounts the fresh draft's editor — no full document load needed.
			await goto(`/templates/${next.id}`);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to create new version';
		} finally {
			saving = false;
		}
	}

	function handleRun() {
		if (!template?.published) return;
		runDraftLocked = false;
		runDialogOpen = true;
	}

	// Draft dev-run: open the same dialog with the mode locked to 'draft'. The
	// backend compiles the draft per-launch (nothing is published); a compile
	// failure comes back on the create POST and lands in onRunCompileError.
	// Private sub-workflows are excluded (toolbar prop gate + guard here):
	// the backend 400s every standalone run of a private template, so the
	// affordance would dead-end after the user filled the form.
	let runDraftLocked = $state(false);
	function handleRunDraft() {
		if (!template || template.published || template.visibility === 'private') return;
		runDraftLocked = true;
		runDialogOpen = true;
	}

	// Surface a draft dev-run's compile failure through the SAME plumbing a
	// failed publish uses (error banner + red canvas rings via compileErrors).
	function onRunCompileError(e: CompileApiError) {
		runDialogOpen = false;
		compileErrors.set(e.compileErrors);
		error = `${e.message} — ${e.compileErrors.length} issue${e.compileErrors.length === 1 ? '' : 's'} highlighted on the canvas`;
	}

	// Throw away this unpublished draft (confirmed via the dialog below). The
	// backend restores the parent version as the chain head — navigate there;
	// a never-published v1 draft deletes the whole template → back to the list.
	async function handleDiscardDraft() {
		if (!template || template.published || discarding) return;
		try {
			discarding = true;
			const res = await discardDraft(template.id);
			discardConfirmOpen = false;
			if (res.restored_head) {
				// In-app nav: the route's `{#key}` remounts the editor on the
				// restored head, tearing this draft's Yjs session down.
				await goto(`/templates/${res.restored_head.id}`);
			} else {
				await goto('/templates');
			}
		} catch (e) {
			discardConfirmOpen = false;
			error = e instanceof Error ? e.message : 'Failed to discard draft';
		} finally {
			discarding = false;
		}
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

	// Open the Notes sheet, fetching-or-creating the template-family's singleton
	// page on first open. Keyed on the chain-root family id so all versions of a
	// workflow share one page (ensureAttachedPage is an idempotent upsert). The
	// sheet renders immediately; the PageEditor mounts once notesPage resolves.
	async function openNotes() {
		notesOpen = true;
		if (!template || notesPage || notesLoading) return;
		try {
			notesLoading = true;
			notesError = null;
			notesPage = await ensureAttachedPage('template', templateFamilyId(template));
		} catch (e) {
			notesError = e instanceof Error ? e.message : 'Failed to open notes';
		} finally {
			notesLoading = false;
		}
	}

	// Notes editability rides the template's own effective role — NOT
	// `published`. The server's WS handler bypasses the published gate for
	// attached pages on purpose (Notes stay editable on a published template),
	// so threading `published` here would wrongly lock the editor.
	const notesEditable = $derived(roleAtLeast(template?.my_effective_role, 'editor'));

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

	// Page-level editor shortcuts: Cmd/Ctrl+Z → undo, Shift+Cmd/Ctrl+Z or
	// Ctrl+Y → redo, Cmd/Ctrl+C/V/D → copy/paste/duplicate the canvas
	// selection. All skipped when the keystroke targets a text field
	// (input/textarea/contenteditable — incl. CodeMirror) so native text
	// editing keeps working. Copy is a pure read and works on published
	// versions too (the module clipboard exists precisely so a known-good
	// published graph can be copied into another template's draft); every
	// mutating shortcut stays draft-gated.
	function isTextEditingTarget(t: EventTarget | null): boolean {
		return (
			t instanceof HTMLInputElement ||
			t instanceof HTMLTextAreaElement ||
			t instanceof HTMLSelectElement ||
			(t instanceof HTMLElement && t.isContentEditable)
		);
	}

	// Current canvas selection → doc-detached clipboard payload. Null when
	// nothing is selected (the keydown handler then lets the browser default
	// through). Multi-selection wins over the single property-panel id.
	function snapshotSelection(): GraphClipboard | null {
		const ids =
			selectedNodeIds.length > 0 ? selectedNodeIds : selectedNodeId ? [selectedNodeId] : [];
		if (ids.length === 0) return null;
		const clip = binding.copySubgraph(ids);
		return clip.nodes.length > 0 ? clip : null;
	}

	async function pasteNodes(clip: GraphClipboard, offset: { x: number; y: number }) {
		// One transaction inside pasteSubgraph → a single Cmd+Z reverts it all.
		const newIds = binding.pasteSubgraph(clip, offset);
		if (newIds.length === 0) return;
		// Hand the selection to the clones once the canvas has synced the new
		// graph (its $effect.pre runs pre-render; tick() awaits that flush).
		await tick();
		canvasRef?.setSelectedNodes(newIds);
		selectedNodeIds = newIds;
		selectedNodeId = newIds.length === 1 ? newIds[0] : null;
	}

	function handleEditorKeydown(e: KeyboardEvent) {
		// The run sheet can sit over a MUTABLE draft (Run draft); its modal
		// traps focus but not keydown propagation, so without this gate
		// Cmd+Z/V/D from a button/label inside the sheet would invisibly
		// mutate the canvas behind the modal (silently corrupting the graph
		// the draft run is about to compile).
		if (runDialogOpen) return;
		if (!e.metaKey && !e.ctrlKey) return;
		if (isTextEditingTarget(e.target)) return;
		const key = e.key.toLowerCase();
		if (key === 'c') {
			// Don't hijack copy while the user has real text selected on the page.
			if (window.getSelection()?.toString()) return;
			const clip = snapshotSelection();
			if (clip) {
				e.preventDefault();
				setClipboard(clip);
			}
			return;
		}
		// Everything below mutates the doc — draft only.
		if (template?.published) return;
		if (key === 'z' || key === 'y') {
			e.preventDefault();
			if (key === 'y' || e.shiftKey) {
				binding.redo();
			} else {
				binding.undo();
			}
			return;
		}
		if (key === 'v') {
			const clip = getClipboard();
			if (clip) {
				e.preventDefault();
				void pasteNodes(clip, nextPasteOffset());
			}
			return;
		}
		if (key === 'd') {
			const clip = snapshotSelection();
			if (clip) {
				e.preventDefault();
				// One-gesture duplicate — deliberately does NOT touch the copy
				// clipboard, so Cmd+D between a copy and its paste is harmless.
				void pasteNodes(clip, { x: 24, y: 24 });
			}
			return;
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

	// One-shot per component instance (the `{#key}` remount IS the per-template
	// re-run); no reactive dependency intended.
	void load();

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

<svelte:window onkeydown={handleEditorKeydown} />

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
				onpreview={handlePreview}
				onnewversion={handleNewVersion}
				ondiscard={template && !template.published
					? () => (discardConfirmOpen = true)
					: undefined}
				onrun={handleRun}
				onrundraft={template && !template.published && template.visibility !== 'private'
					? handleRunDraft
					: undefined}
				ontests={() => (testsPanelOpen = true)}
				onnotes={template ? openNotes : undefined}
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
					bind:this={canvasRef}
					graph={binding.graph}
					readonly={template?.published ?? false}
					onselect={handleNodeSelect}
					onSelectionChange={(ids) => (selectedNodeIds = ids)}
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

<Sheet.Root open={notesOpen} onOpenChange={(o: boolean) => (notesOpen = o)}>
	<SheetContent class="flex w-full max-w-2xl flex-col p-0 sm:max-w-2xl">
		<SheetTitle class="border-b border-border px-4 py-3 text-sm font-medium">Notes</SheetTitle>
		<div class="min-h-0 flex-1 overflow-hidden px-4 py-3">
			{#if notesError}
				<div class="text-sm text-destructive">{notesError}</div>
			{:else if notesLoading || !notesPage}
				<div class="text-sm text-muted-foreground">Loading notes…</div>
			{:else}
				{#key notesPage.id}
					<PageEditor pageId={notesPage.id} editable={notesEditable} />
				{/key}
			{/if}
		</div>
	</SheetContent>
</Sheet.Root>

<PublishGateModal
	open={publishGate !== null}
	failingTests={publishGate ?? []}
	onclose={() => {
		publishGate = null;
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
	lockMode={runDraftLocked ? 'draft' : null}
	graph={runDraftLocked ? binding.graph : null}
	oncompileerror={onRunCompileError}
/>

<Dialog.Root bind:open={discardConfirmOpen}>
	<Dialog.Content class="sm:max-w-md">
		<Dialog.Header>
			<Dialog.Title>Discard this draft?</Dialog.Title>
			<Dialog.Description>
				{#if template?.parent_id}
					Draft v{template.version} and its unpublished changes are deleted permanently;
					v{template.version - 1} becomes the latest version again.
				{:else}
					This draft has never been published, so discarding it deletes the whole
					template permanently.
				{/if}
			</Dialog.Description>
		</Dialog.Header>
		<Dialog.Footer>
			<Button variant="outline" onclick={() => (discardConfirmOpen = false)}>Cancel</Button>
			<Button
				variant="destructive"
				disabled={discarding}
				data-testid="btn-confirm-discard-draft"
				onclick={handleDiscardDraft}
			>
				{discarding ? 'Discarding…' : 'Discard draft'}
			</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>

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
