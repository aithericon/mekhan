<script lang="ts">
	import { page } from '$app/state';
	import { replaceState, goto } from '$app/navigation';
	import { resolveRoute } from '$app/paths';
	import { onDestroy } from 'svelte';
	import IdeToolbar from '$lib/components/ide/IdeToolbar.svelte';
	import FileTree from '$lib/components/ide/FileTree.svelte';
	import EditorTabs from '$lib/components/ide/EditorTabs.svelte';
	import NodeConfigPanel from '$lib/components/ide/NodeConfigPanel.svelte';
	import HumanTaskFormEditor from '$lib/components/ide/HumanTaskFormEditor.svelte';
	import LlmStepIdeEditor from '$lib/components/ide/LlmStepIdeEditor.svelte';
	import CreateInstanceDialog from '$lib/components/instances/CreateInstanceDialog.svelte';
	import CopyButton from '$lib/components/ui/copy-button/CopyButton.svelte';
	import { getSession, releaseSession } from '$lib/yjs/session-store';
	import { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import {
		getTemplate,
		publishTemplate,
		createNewVersion,
		uploadFile,
		updateTemplate,
		type Template
	} from '$lib/api/client';
	import { fetchNodeScopes, type ScopeEntry } from '$lib/editor/guard-scope';
	import { setWorkflowDefinitions } from '$lib/editor/workflow-definitions.svelte';
	import type { CodeEditorApi } from '$lib/components/editor/panels/shared/CollabCodeEditor.svelte';
	import type { components } from '$lib/api/schema';

	type GuardDiagnosticDto = components['schemas']['GuardDiagnosticDto'];

	const templateId = $derived(page.params.id!);

	let template = $state<Template | null>(null);
	let error = $state<string | null>(null);
	let runDialogOpen = $state(false);

	// Per-node input scope for the step reference panel + every other panel
	// that wants a RefPicker (Decision, Loop, HumanTask, AutomatedStep). Same
	// `POST /api/analyze`-backed loader the canvas-mode property panel uses,
	// so what the picker shows is exactly what the compiler resolves.
	let nodeScopes = $state<Map<string, ScopeEntry[]>>(new Map());
	// Server-authoritative globals (resources + assets) from the analyze
	// response. Passed to NodeConfigPanel as `globalsScope`.
	let globalsScope = $state<ScopeEntry[]>([]);
	let scopeBusy = $state(false);
	// Surfaced from the analyzer so an empty picker can explain itself: when
	// `graphOk` is false (dangling edge, missing End, …) or the request
	// failed, the IDE renders a banner instead of letting the user wonder why
	// no refs appear.
	let graphOk = $state(true);
	let scopeRequestFailed = $state(false);
	let scopeDiagnostics = $state<GuardDiagnosticDto[]>([]);
	async function refreshScopes() {
		scopeBusy = true;
		try {
			const result = await fetchNodeScopes(binding.graph, {
				templateId,
				workspaceId: template?.workspace_id
			});
			nodeScopes = result.scopes;
			globalsScope = result.globalsScope;
			graphOk = result.graphOk;
			scopeRequestFailed = result.requestFailed;
			scopeDiagnostics = result.diagnostics;
		} finally {
			scopeBusy = false;
		}
	}

	// Active editor's insert-at-cursor API. Re-registered on each tab switch
	// (each tab mounts a fresh CollabCodeEditor via {#key activeTab}). When
	// null, the reference panel disables its "insert" affordance.
	let editorApi = $state<CodeEditorApi | null>(null);

	// Yjs session
	const session = getSession(templateId);
	const binding = new YjsGraphBinding(session.doc);

	// Tab state (local, per-user)
	type TabInfo = { nodeId: string; filename: string; label: string };
	let openTabs = $state<TabInfo[]>([]);
	let activeTabKey = $state<string | null>(null);
	let selectedNodeId = $state<string | null>(null);
	let selectedFile = $state<{ nodeId: string; filename: string } | undefined>(undefined);

	// Derive whether the selected node is a human_task (show form editor instead of code tabs)
	const selectedNodeData = $derived(
		selectedNodeId ? binding.graph.nodes.find((n) => n.id === selectedNodeId)?.data ?? null : null
	);
	const showHumanTaskEditor = $derived(
		selectedNodeData?.type === 'human_task' && !activeTabKey
	);
	const showLlmStepEditor = $derived(
		selectedNodeData?.type === 'automated_step' &&
			(selectedNodeData as { executionSpec?: { backendType?: string } })?.executionSpec?.backendType ===
				'llm' &&
			!activeTabKey
	);

	function tabKey(nodeId: string, filename: string): string {
		return `${nodeId}:${filename}`;
	}

	async function load() {
		try {
			template = await getTemplate(templateId);
			// Stash workflow `definitions` so the derived-port twin can resolve
			// `$ref` response_formats (they're absent from the Yjs doc).
			setWorkflowDefinitions(
				(template?.graph as { definitions?: Record<string, unknown> } | undefined)?.definitions ??
					null
			);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load template';
		}
	}

	async function handlePublish() {
		if (!template || template.published) return;
		try {
			template = await publishTemplate(template.id);
			void refreshScopes();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to publish';
		}
	}

	async function handleNewVersion() {
		if (!template || !template.published) return;
		try {
			const next = await createNewVersion(template.id);
			// Full document load (not `goto`): the Yjs session + binding are
			// created once at script top from the initial templateId, so a
			// param-only nav would leave the IDE pinned to the published
			// version's doc. See TemplateVersionMenu.select for the same reason.
			window.location.assign(`/templates/${next.id}/ide`);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to create new version';
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
		// Optimistic: reflect immediately, roll back on failure.
		template = { ...template, name };
		try {
			template = await updateTemplate(templateId, { name });
		} catch (e) {
			template = prev;
			error = e instanceof Error ? e.message : 'Rename failed';
		}
	}

	function syncUrlState() {
		const params = new URLSearchParams();
		if (activeTabKey) params.set('file', activeTabKey);
		if (selectedNodeId) params.set('node', selectedNodeId);
		const qs = params.toString();
		const path = resolveRoute('/templates/[id]/ide', { id: templateId });
		replaceState(`${path}${qs ? '?' + qs : ''}`, {});
	}

	function handleSelectNode(nodeId: string) {
		selectedNodeId = nodeId;
		selectedFile = undefined;
		// Clear active tab so center panel shows node-specific editor (human task form / LLM step).
		const nodeData = binding.graph.nodes.find((n) => n.id === nodeId)?.data;
		const isLlmStep =
			nodeData?.type === 'automated_step' &&
			(nodeData as { executionSpec?: { backendType?: string } })?.executionSpec?.backendType ===
				'llm';
		if (nodeData?.type === 'human_task' || isLlmStep) {
			activeTabKey = null;
		}
		syncUrlState();
	}

	function handleSelectFile(nodeId: string, filename: string) {
		selectedNodeId = nodeId;
		selectedFile = { nodeId, filename };

		const key = tabKey(nodeId, filename);
		const existing = openTabs.find((t) => tabKey(t.nodeId, t.filename) === key);
		if (!existing) {
			const node = binding.graph.nodes.find((n) => n.id === nodeId);
			openTabs = [...openTabs, { nodeId, filename, label: node?.data.label ?? nodeId }];
		}
		activeTabKey = key;
		syncUrlState();
	}

	async function handleUploadFile(nodeId: string, file: File) {
		try {
			const result = await uploadFile(templateId, nodeId, file);
			// Store the S3 key as the Y.Text content
			binding.createFile(nodeId, file.name, result.key);
			handleSelectFile(nodeId, file.name);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Upload failed';
		}
	}

	function handleCreateFile(nodeId: string) {
		const filename = prompt('File name:', 'main.py');
		if (!filename) return;
		binding.createFile(nodeId, filename, '');
		handleSelectFile(nodeId, filename);
	}

	function handleDeleteFile(nodeId: string, filename: string) {
		binding.deleteFile(nodeId, filename);
		const key = tabKey(nodeId, filename);
		openTabs = openTabs.filter((t) => tabKey(t.nodeId, t.filename) !== key);
		if (activeTabKey === key) {
			activeTabKey = openTabs.length > 0 ? tabKey(openTabs[0].nodeId, openTabs[0].filename) : null;
		}
		if (selectedFile?.nodeId === nodeId && selectedFile?.filename === filename) {
			selectedFile = undefined;
		}
		syncUrlState();
	}

	function handleRenameFile(nodeId: string, oldName: string, newName: string) {
		binding.renameFile(nodeId, oldName, newName);
		const oldKey = tabKey(nodeId, oldName);
		openTabs = openTabs.map((t) =>
			tabKey(t.nodeId, t.filename) === oldKey ? { ...t, filename: newName } : t
		);
		if (activeTabKey === oldKey) {
			activeTabKey = tabKey(nodeId, newName);
		}
	}

	function handleCloseTab(key: string) {
		openTabs = openTabs.filter((t) => tabKey(t.nodeId, t.filename) !== key);
		if (activeTabKey === key) {
			activeTabKey = openTabs.length > 0 ? tabKey(openTabs[0].nodeId, openTabs[0].filename) : null;
		}
		syncUrlState();
	}

	function handleSelectTab(key: string) {
		activeTabKey = key;
		const tab = openTabs.find((t) => tabKey(t.nodeId, t.filename) === key);
		if (tab) {
			selectedNodeId = tab.nodeId;
			selectedFile = { nodeId: tab.nodeId, filename: tab.filename };
		}
		syncUrlState();
	}

	// Restore state from URL query params once the Y.Doc has synced
	let initialStateApplied = false;
	$effect(() => {
		if (initialStateApplied || binding.graph.nodes.length === 0) return;
		initialStateApplied = true;

		const fileParam = page.url.searchParams.get('file');
		const nodeParam = page.url.searchParams.get('node');

		if (nodeParam) {
			selectedNodeId = nodeParam;
			// If it's a human_task or LLM step and no file param, show the
			// node-specific center editor.
			const nodeData = binding.graph.nodes.find((n) => n.id === nodeParam)?.data;
			const isLlmStep =
				nodeData?.type === 'automated_step' &&
				(nodeData as { executionSpec?: { backendType?: string } })?.executionSpec?.backendType ===
					'llm';
			if ((nodeData?.type === 'human_task' || isLlmStep) && !fileParam) {
				activeTabKey = null;
			}
		}
		if (fileParam) {
			const [nodeId, ...rest] = fileParam.split(':');
			const filename = rest.join(':');
			if (nodeId && filename) handleSelectFile(nodeId, filename);
		}
	});

	$effect(() => {
		load();
	});

	// Keep every panel's scope fresh. /api/analyze derives scope from the live
	// graph, so *any* edit — including adding a port field on another node —
	// can change a step's scope. Refetch debounced after edits settle (not
	// just on node switch, which is why a freshly added field looked like it
	// "didn't show up"). The first post-sync update also corrects the initial
	// fetch if it raced ahead of the doc.
	let scopeTimer: ReturnType<typeof setTimeout> | undefined;
	function scheduleScopeRefresh() {
		clearTimeout(scopeTimer);
		scopeTimer = setTimeout(() => void refreshScopes(), 500);
	}
	$effect(() => {
		void refreshScopes();
		session.doc.on('update', scheduleScopeRefresh);
		return () => {
			session.doc.off('update', scheduleScopeRefresh);
			clearTimeout(scopeTimer);
		};
	});

	onDestroy(() => {
		binding.destroy();
		releaseSession(templateId);
	});
</script>

<div class="flex h-full flex-col">
	<IdeToolbar
		templateName={template?.name ?? 'Loading...'}
		{templateId}
		published={template?.published ?? false}
		version={template?.version}
		awareness={session.awareness}
		provider={session.provider}
		onPublish={handlePublish}
		onNewVersion={handleNewVersion}
		onRun={handleRun}
		onRename={handleRename}
	/>

	{#if error}
		<div class="flex items-center gap-2 border-b border-amber-200 bg-amber-50 px-4 py-2 text-sm text-amber-800">
			<span class="flex-1">{error}</span>
			<CopyButton text={error} title="Copy error" class="text-amber-700 hover:text-amber-900" />
			<button type="button" class="underline" onclick={() => (error = null)}>dismiss</button>
		</div>
	{/if}

	<!-- Analyzer status: surfaced so an empty reference panel is explained.
	     `graphOk: false` is a compiler verdict (dangling edge, missing End,
	     cycle, …); `scopeRequestFailed` is a transport-level fault. -->
	{#if !scopeBusy && (!graphOk || scopeRequestFailed)}
		<div
			class="flex items-center justify-between gap-3 border-b border-amber-200 bg-amber-50 px-4 py-1.5 text-sm text-amber-900"
			data-testid="ide-analyzer-banner"
		>
			<span>
				{#if scopeRequestFailed}
					<strong>Variable references unavailable</strong> — the analyzer didn't
					respond. Click Refresh in the Reference panel to retry.
				{:else}
					<strong>Variable references unavailable</strong> — the graph isn't a
					complete flow yet
					{#if scopeDiagnostics.length > 0}
						({scopeDiagnostics.length} diagnostic{scopeDiagnostics.length === 1 ? '' : 's'})
					{/if}.
					Wire every node to a Start/End, resolve dangling edges, then it
					recomputes automatically.
				{/if}
			</span>
			{#if scopeDiagnostics.length > 0}
				<details class="text-sm">
					<summary class="cursor-pointer underline">Show diagnostics</summary>
					<ul class="mt-1 space-y-0.5">
						{#each scopeDiagnostics as d, i (`${d.node_id}:${i}`)}
							<li>
								<code class="font-mono">{d.kind}</code> · {d.message}
								{#if d.node_id}
									<span class="text-muted-foreground">({d.node_id})</span>
								{/if}
							</li>
						{/each}
					</ul>
				</details>
			{/if}
		</div>
	{/if}

	<div class="flex flex-1 overflow-hidden">
		<div class="w-[200px] shrink-0">
			<FileTree
				{binding}
				{selectedFile}
				{selectedNodeId}
				onSelectFile={handleSelectFile}
				onSelectNode={handleSelectNode}
				onCreateFile={handleCreateFile}
				onUploadFile={handleUploadFile}
				onDeleteFile={handleDeleteFile}
				onRenameFile={handleRenameFile}
			/>
		</div>

		<div class="flex-1 overflow-hidden">
			{#if showHumanTaskEditor && selectedNodeId}
				<HumanTaskFormEditor
					{binding}
					nodeId={selectedNodeId}
					readonly={template?.published ?? false}
				/>
			{:else if showLlmStepEditor && selectedNodeId}
				<LlmStepIdeEditor
					{binding}
					nodeId={selectedNodeId}
					readonly={template?.published ?? false}
					scope={nodeScopes.get(selectedNodeId) ?? []}
				/>
			{:else}
				<EditorTabs
					tabs={openTabs}
					activeTab={activeTabKey}
					{binding}
					awareness={session.awareness}
					provider={session.provider}
					onCloseTab={handleCloseTab}
					onSelectTab={handleSelectTab}
					onEditorReady={(api) => (editorApi = api)}
				/>
			{/if}
		</div>

		<div class="w-[320px] shrink-0">
			{#if selectedNodeId}
				<NodeConfigPanel
					{binding}
					nodeId={selectedNodeId}
					readonly={template?.published ?? false}
					scope={nodeScopes.get(selectedNodeId) ?? []}
					{globalsScope}
					scopeBusy={scopeBusy}
					onRefreshScope={refreshScopes}
					oninsertref={editorApi ? (s) => editorApi?.insertAtCursor(s) : undefined}
				/>
			{:else}
				<div class="flex h-full items-center justify-center border-l border-border bg-card text-sm text-muted-foreground">
					Select a node to configure
				</div>
			{/if}
		</div>
	</div>
</div>

<CreateInstanceDialog
	bind:open={runDialogOpen}
	templateId={template?.id ?? null}
	oncreated={onInstanceCreated}
/>
