<script lang="ts">
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import {
		LabCanvas,
		Timeline,
		EventLog,
		Inspector,
		TokenDetailSheet,
		TransitionScriptSheet,
		AnalysisPanel,
		ServicesPanel,
		NetTreeSidebar,
		ScenarioEditor
	} from '$lib/components/petri';
	import type { NetTreeNode, NetMeta } from '$lib/components/petri/NetTreeSidebar.svelte';
	import { createPetriStore } from '$lib/stores/petri.svelte';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import Play from '@lucide/svelte/icons/play';
	import Pause from '@lucide/svelte/icons/pause';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import FileText from '@lucide/svelte/icons/file-text';
	import PanelLeftClose from '@lucide/svelte/icons/panel-left-close';
	import PanelLeftOpen from '@lucide/svelte/icons/panel-left-open';

	const PETRI_URL = '/petri';

	const netId = $derived($page.params.id as string);
	const petriStore = $derived(createPetriStore(netId));

	// ── Layout state ────────────────────────────────────────────────────
	let showNetTree = $state(true);
	let rightTab = $state<'inspector' | 'services' | 'analysis'>('inspector');
	let showScenarioEditor = $state(false);

	// ── Script sheet state ──────────────────────────────────────────────
	let showScriptSheet = $state(false);

	// ── Token sheet state ───────────────────────────────────────────────
	let showTokenSheet = $state(false);

	// ── Net tree sidebar data ───────────────────────────────────────────
	let netsMeta = $state<Array<{ net_id: string; status: string; in_memory: boolean; parent_net_id?: string }>>([]);
	let netTreeFilter = $state<'active' | 'all'>('active');

	const netTree = $derived.by((): NetTreeNode[] => {
		const filtered = netTreeFilter === 'active'
			? netsMeta.filter(n => n.status === 'running' || n.status === 'created')
			: netsMeta;

		const nodeMap = new Map<string, NetTreeNode>();
		const roots: NetTreeNode[] = [];

		for (const n of filtered) {
			nodeMap.set(n.net_id, {
				meta: { netId: n.net_id, label: n.net_id, status: n.status, inMemory: n.in_memory, parentNetId: n.parent_net_id },
				children: []
			});
		}

		for (const node of nodeMap.values()) {
			const parentId = node.meta.parentNetId;
			if (parentId && nodeMap.has(parentId)) {
				nodeMap.get(parentId)!.children.push(node);
			} else {
				roots.push(node);
			}
		}

		return roots;
	});

	async function fetchNetsMeta() {
		try {
			const res = await fetch(`${PETRI_URL}/api/nets/metadata`);
			if (res.ok) netsMeta = await res.json();
		} catch { /* non-critical */ }
	}

	// ── Derived inspector data ──────────────────────────────────────────
	const selectedElement = $derived(petriStore.selectedElement);
	const placeDetails = $derived(petriStore.getSelectedPlaceDetails());
	const transitionDetails = $derived(petriStore.getSelectedTransitionDetails());
	const tokenDetails = $derived(petriStore.getSelectedTokenDetails());
	const eventDetails = $derived(petriStore.getSelectedEventDetails());
	const groupDetails = $derived(petriStore.getSelectedGroupDetails());

	// ── Script sheet derived data ───────────────────────────────────────
	const scriptTransition = $derived.by(() => {
		if (!showScriptSheet || !transitionDetails) return null;
		return transitionDetails;
	});

	// ── Token sheet derived data ────────────────────────────────────────
	const tokenSheetData = $derived.by(() => {
		if (!showTokenSheet || !tokenDetails) return null;
		return tokenDetails;
	});

	// ── Lifecycle ───────────────────────────────────────────────────────
	$effect(() => {
		petriStore.init();
		fetchNetsMeta();
		return () => petriStore.destroy();
	});

	// ── Actions ─────────────────────────────────────────────────────────
	function handleOpenScript() {
		if (selectedElement?.type === 'transition') {
			showScriptSheet = true;
		}
	}

	function handleViewToken() {
		if (selectedElement?.type === 'token') {
			showTokenSheet = true;
		}
	}

	function handleToggleRunMode() {
		const next = petriStore.runMode === 'running' ? 'stopped' : 'running';
		petriStore.setRunMode(next);
	}

	async function handleDeleteNet(id: string) {
		if (!confirm(`Delete net "${id}"?`)) return;
		try {
			await fetch(`${PETRI_URL}/api/nets/${id}`, { method: 'DELETE' });
			await fetchNetsMeta();
			if (id === netId) goto('/nets');
		} catch { /* ignore */ }
	}
</script>

<div class="flex h-full flex-col bg-background">
	<!-- Header -->
	<div class="flex items-center gap-3 border-b border-border px-4 py-2 shrink-0">
		<Button variant="ghost" size="icon-sm" href="/nets">
			<ArrowLeft class="size-4" />
		</Button>
		<Button variant="ghost" size="icon-sm" onclick={() => (showNetTree = !showNetTree)}>
			{#if showNetTree}
				<PanelLeftClose class="size-4" />
			{:else}
				<PanelLeftOpen class="size-4" />
			{/if}
		</Button>
		<div class="flex items-center gap-2">
			<span class="font-mono text-sm font-medium">{netId}</span>
			{#if petriStore.runMode}
				<Badge
					class={petriStore.runMode === 'running'
						? 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400'
						: 'bg-gray-100 text-gray-700 dark:bg-gray-800 dark:text-gray-400'}
				>
					{petriStore.runMode}
				</Badge>
			{/if}
		</div>
		<div class="ml-auto flex items-center gap-1">
			<Button variant="outline" size="sm" onclick={() => (showScenarioEditor = true)}>
				<FileText class="size-3.5" /> Scenario
			</Button>
			<Button variant="outline" size="sm" onclick={() => petriStore.reset()}>
				<RotateCcw class="size-3.5" /> Reset
			</Button>
			<Button variant="outline" size="sm" onclick={handleToggleRunMode}>
				{#if petriStore.runMode === 'running'}
					<Pause class="size-3.5" /> Pause
				{:else}
					<Play class="size-3.5" /> Start
				{/if}
			</Button>
			<Button variant="outline" size="sm" onclick={() => petriStore.evaluate()}>
				<RotateCcw class="size-3.5" /> Eval
			</Button>
			<Button variant="ghost" size="icon-sm" onclick={fetchNetsMeta}>
				<RefreshCw class="size-3.5" />
			</Button>
		</div>
	</div>

	<!-- Main content: three-column layout -->
	<div class="flex flex-1 min-h-0">
		<!-- Left sidebar: Net tree -->
		{#if showNetTree}
			<div class="w-52 border-r border-border shrink-0">
				<NetTreeSidebar
					tree={netTree}
					activeNetId={netId}
					statusFilter={netTreeFilter}
					onSelectNet={(id) => goto(`/nets/${id}`)}
					onRemoveNet={handleDeleteNet}
					onRefresh={fetchNetsMeta}
					onToggleFilter={() => (netTreeFilter = netTreeFilter === 'active' ? 'all' : 'active')}
				/>
			</div>
		{/if}

		<!-- Center: Canvas + Timeline -->
		<div class="flex flex-1 flex-col min-w-0">
			<!-- Canvas -->
			<div class="flex-1 relative min-h-0">
				{#if petriStore.topology}
					<LabCanvas
						topology={petriStore.topology}
						marking={petriStore.projectedMarking}
						bridgedOutTokens={petriStore.bridgedOutTokens}
						{netId}
						enabledTransitions={Object.entries(petriStore.transitionStatuses)
							.filter(([_, s]) => s === 'enabled')
							.map(([id]) => id)}
						transitionStatuses={petriStore.transitionStatuses}
						groups={petriStore.currentGroups}
						selectedElementId={selectedElement?.type === 'place' ? selectedElement.id
							: selectedElement?.type === 'transition' ? selectedElement.id
							: selectedElement?.type === 'group' ? selectedElement.id
							: null}
						spotlight={petriStore.eventSpotlight}
						markingDiff={petriStore.markingDiff}
						onFireTransition={(id) => petriStore.fireTransition(id)}
						onSelectPlace={(id) => petriStore.selectPlace(id)}
						onSelectTransition={(id) => petriStore.selectTransition(id)}
						onSelectToken={(placeId, tokenId) => petriStore.selectToken(placeId, tokenId)}
						onSelectGroup={(id) => petriStore.selectGroup(id)}
						onSelectRemoteNet={(id, label, targets, sources, childNetIds) => petriStore.selectRemoteNet(id, label, targets, sources, childNetIds)}
					/>
				{:else if petriStore.error}
					<div class="flex items-center justify-center h-full text-sm text-destructive">
						{petriStore.error}
					</div>
				{:else}
					<div class="flex items-center justify-center h-full text-sm text-muted-foreground">
						Loading topology...
					</div>
				{/if}
			</div>

			<!-- Timeline -->
			<div class="shrink-0 border-t border-border">
				<Timeline
					events={petriStore.events ?? []}
					currentIndex={petriStore.replayIndex}
					onIndexChange={(i) => petriStore.setReplayIndex(i)}
					evaluating={petriStore.evaluating}
					runMode={petriStore.runMode}
					onEvaluate={() => petriStore.evaluate()}
					onToggleRunMode={handleToggleRunMode}
					onHibernate={() => petriStore.hibernate()}
				/>
			</div>
		</div>

		<!-- Right sidebar: Inspector/Services/Analysis tabs -->
		<div class="w-80 border-l border-border shrink-0 flex flex-col">
			<!-- Tab bar -->
			<div class="flex border-b border-border shrink-0">
				<button
					class="flex-1 px-2 py-1.5 text-xs font-medium transition-colors border-b-2
						{rightTab === 'inspector'
							? 'border-primary text-foreground'
							: 'border-transparent text-muted-foreground hover:text-foreground'}"
					onclick={() => (rightTab = 'inspector')}
				>
					Inspector
				</button>
				<button
					class="flex-1 px-2 py-1.5 text-xs font-medium transition-colors border-b-2
						{rightTab === 'services'
							? 'border-primary text-foreground'
							: 'border-transparent text-muted-foreground hover:text-foreground'}"
					onclick={() => (rightTab = 'services')}
				>
					Services
				</button>
				<button
					class="flex-1 px-2 py-1.5 text-xs font-medium transition-colors border-b-2
						{rightTab === 'analysis'
							? 'border-primary text-foreground'
							: 'border-transparent text-muted-foreground hover:text-foreground'}"
					onclick={() => (rightTab = 'analysis')}
				>
					Analysis
				</button>
			</div>

			<!-- Tab content -->
			<div class="flex-1 min-h-0 overflow-hidden">
				{#if rightTab === 'inspector'}
					<Inspector
						{selectedElement}
						{placeDetails}
						{transitionDetails}
						{tokenDetails}
						{eventDetails}
						{groupDetails}
						loading={petriStore.loading}
						getTransitionName={petriStore.getTransitionName}
						getPlaceName={petriStore.getPlaceName}
						onSelectPlace={(id) => petriStore.selectPlace(id)}
						onSelectTransition={(id) => petriStore.selectTransition(id)}
						onSelectToken={(placeId, tokenId) => petriStore.selectToken(placeId, tokenId)}
						onSelectGroup={(id) => petriStore.selectGroup(id)}
						onSelectEvent={(seq) => petriStore.selectEvent(seq)}
						onClearSelection={() => petriStore.clearSelection()}
						onInjectToken={(placeId, data) => petriStore.injectToken(placeId, data)}
						onSetReplayIndex={(idx) => petriStore.setReplayIndex(idx)}
						onOpenScript={handleOpenScript}
						onViewToken={handleViewToken}
						onNavigateToChild={(id) => goto(`/nets/${id}`)}
					/>
				{:else if rightTab === 'services'}
					<ServicesPanel
						services={petriStore.services}
						onRefresh={() => petriStore.fetchServices()}
					/>
				{:else if rightTab === 'analysis'}
					<AnalysisPanel
						report={petriStore.analysisReport}
						onRefresh={() => petriStore.fetchAnalysis()}
						onSelectNode={(nodeId, nodeType) => {
							if (nodeType === 'place') petriStore.selectPlace(nodeId);
							else if (nodeType === 'transition') petriStore.selectTransition(nodeId);
						}}
					/>
				{/if}
			</div>
		</div>

		<!-- Far right: Event Log -->
		<div class="w-72 border-l border-border shrink-0 flex flex-col">
			<EventLog
				events={petriStore.events ?? []}
				currentIndex={petriStore.replayIndex}
				onSelectEvent={(idx) => petriStore.setReplayIndex(idx)}
				onInspectEvent={(seq) => petriStore.selectEvent(seq)}
				getTransitionName={petriStore.getTransitionName}
				getPlaceName={petriStore.getPlaceName}
			/>
		</div>
	</div>

	<!-- Overlays -->

	<!-- Transition Script Sheet (bottom) -->
	{#if showScriptSheet && scriptTransition}
		<TransitionScriptSheet
			transition={scriptTransition.transition}
			inputPorts={scriptTransition.transition.input_ports ?? []}
			outputPorts={scriptTransition.transition.output_ports ?? []}
			guard={scriptTransition.transition.guard ?? null}
			script={scriptTransition.transition.script ?? ''}
			status={selectedElement?.type === 'transition' ? petriStore.transitionStatuses[selectedElement.id] : undefined}
			effectHandlerId={scriptTransition.transition.effect_handler_id}
			open={showScriptSheet}
			onClose={() => (showScriptSheet = false)}
			onSaveScript={async (tid, script, guard) => {
				await petriStore.saveTransitionScript(tid, script, guard);
			}}
		/>
	{/if}

	<!-- Token Detail Sheet (right) -->
	{#if showTokenSheet && tokenSheetData}
		<TokenDetailSheet
			token={tokenSheetData.token}
			placeName={tokenSheetData.placeName}
			open={showTokenSheet}
			onClose={() => (showTokenSheet = false)}
		/>
	{/if}

	<!-- Scenario Editor (modal) -->
	{#if showScenarioEditor}
		<ScenarioEditor
			store={petriStore}
			{netId}
			onClose={() => (showScenarioEditor = false)}
		/>
	{/if}
</div>
