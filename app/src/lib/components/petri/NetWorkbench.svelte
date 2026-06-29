<script lang="ts" module>
	import type { Snippet } from 'svelte';
	import type { PetriStore } from '$lib/stores/petri.svelte';

	/** Passed to the `header` snippet so callers build their own top bar
	 *  (debug toolbar for /nets, lineage header for an instance Run) without
	 *  re-wiring the store. */
	export type WorkbenchApi = {
		store: PetriStore;
		netTreeOpen: boolean;
		toggleNetTree: () => void;
		openScenario: () => void;
		refreshNets: () => void;
	};
</script>

<script lang="ts">
	import { goto } from '$app/navigation';
	import {
		LabCanvas,
		Timeline,
		EventLog,
		Inspector,
		TokenDetailSheet,
		TransitionScriptSheet,
		AnalysisPanel,
		ServicesPanel,
		MemoryPanel,
		NetTreeSidebar,
		ScenarioEditor
	} from '$lib/components/petri';
	import type { NetTreeNode } from '$lib/components/petri/NetTreeSidebar.svelte';
	import { createPetriStore } from '$lib/stores/petri.svelte';

	const PETRI_URL = '/petri';

	let {
		netId,
		showNetTree = true,
		onSelectNet = (id: string) => goto(`/nets/${id}`),
		onDeleteNet,
		header
	}: {
		netId: string;
		showNetTree?: boolean;
		onSelectNet?: (id: string) => void;
		onDeleteNet?: (id: string) => void;
		header?: Snippet<[WorkbenchApi]>;
	} = $props();

	const petriStore = $derived(createPetriStore(netId));

	// ── Layout state ────────────────────────────────────────────────────
	// Seed the open/closed default from the prop, then own it locally.
	// svelte-ignore state_referenced_locally
	let netTreeOpen = $state(showNetTree);
	let rightTab = $state<'inspector' | 'services' | 'analysis' | 'memory'>('inspector');
	let showScenarioEditor = $state(false);
	let showScriptSheet = $state(false);
	let showTokenSheet = $state(false);

	// ── Net tree sidebar data ───────────────────────────────────────────
	let netsMeta = $state<
		Array<{ net_id: string; status: string; in_memory: boolean; parent_net_id?: string }>
	>([]);
	let netTreeFilter = $state<'active' | 'all'>('active');

	const netTree = $derived.by((): NetTreeNode[] => {
		const filtered =
			netTreeFilter === 'active'
				? netsMeta.filter((n) => n.status === 'running' || n.status === 'created')
				: netsMeta;

		const nodeMap = new Map<string, NetTreeNode>();
		const roots: NetTreeNode[] = [];

		for (const n of filtered) {
			nodeMap.set(n.net_id, {
				meta: {
					netId: n.net_id,
					label: n.net_id,
					status: n.status,
					inMemory: n.in_memory,
					parentNetId: n.parent_net_id
				},
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
		} catch {
			/* non-critical */
		}
	}

	// ── Derived inspector data ──────────────────────────────────────────
	const selectedElement = $derived(petriStore.selectedElement);
	const placeDetails = $derived(petriStore.getSelectedPlaceDetails());
	const transitionDetails = $derived(petriStore.getSelectedTransitionDetails());
	const tokenDetails = $derived(petriStore.getSelectedTokenDetails());
	const eventDetails = $derived(petriStore.getSelectedEventDetails());
	const groupDetails = $derived(petriStore.getSelectedGroupDetails());

	const scriptTransition = $derived.by(() => {
		if (!showScriptSheet || !transitionDetails) return null;
		return transitionDetails;
	});

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
		if (selectedElement?.type === 'transition') showScriptSheet = true;
	}

	function handleViewToken() {
		if (selectedElement?.type === 'token') showTokenSheet = true;
	}

	function handleToggleRunMode() {
		petriStore.setRunMode(petriStore.runMode === 'running' ? 'stopped' : 'running');
	}

	async function handleDeleteNet(id: string) {
		onDeleteNet?.(id);
		await fetchNetsMeta();
	}

	const api: WorkbenchApi = {
		get store() {
			return petriStore;
		},
		get netTreeOpen() {
			return netTreeOpen;
		},
		toggleNetTree: () => (netTreeOpen = !netTreeOpen),
		openScenario: () => (showScenarioEditor = true),
		refreshNets: fetchNetsMeta
	};
</script>

<div class="flex h-full flex-col bg-background">
	{#if header}
		{@render header(api)}
	{/if}

	<!-- Main content: three-column layout -->
	<div class="flex flex-1 min-h-0">
		<!-- Left sidebar: Net tree -->
		{#if netTreeOpen}
			<div class="w-52 border-r border-border shrink-0">
				<NetTreeSidebar
					tree={netTree}
					activeNetId={netId}
					statusFilter={netTreeFilter}
					onSelectNet={(id) => onSelectNet(id)}
					onRemoveNet={handleDeleteNet}
					onRefresh={fetchNetsMeta}
					onToggleFilter={() =>
						(netTreeFilter = netTreeFilter === 'active' ? 'all' : 'active')}
				/>
			</div>
		{/if}

		<!-- Center: Canvas + Timeline -->
		<div class="flex flex-1 flex-col min-w-0">
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
						selectedElementId={selectedElement?.type === 'place'
							? selectedElement.id
							: selectedElement?.type === 'transition'
								? selectedElement.id
								: selectedElement?.type === 'group'
									? selectedElement.id
									: null}
						spotlight={petriStore.eventSpotlight}
						markingDiff={petriStore.markingDiff}
						onFireTransition={(id) => petriStore.fireTransition(id)}
						onSelectPlace={(id) => petriStore.selectPlace(id)}
						onSelectTransition={(id) => petriStore.selectTransition(id)}
						onSelectToken={(placeId, tokenId) => petriStore.selectToken(placeId, tokenId)}
						onSelectGroup={(id) => petriStore.selectGroup(id)}
						onSelectRemoteNet={(id, label, targets, sources, childNetIds) =>
							petriStore.selectRemoteNet(id, label, targets, sources, childNetIds)}
					/>
				{:else if petriStore.error}
					<div class="flex items-center justify-center h-full text-sm text-destructive">
						{petriStore.error}
					</div>
				{:else}
					<div
						class="flex items-center justify-center h-full text-sm text-muted-foreground"
					>
						Loading topology...
					</div>
				{/if}
			</div>

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
			<div class="flex border-b border-border shrink-0">
				<button
					class="flex-1 px-2 py-1.5 text-sm font-medium transition-colors border-b-2
						{rightTab === 'inspector'
						? 'border-primary text-foreground'
						: 'border-transparent text-muted-foreground hover:text-foreground'}"
					onclick={() => (rightTab = 'inspector')}
				>
					Inspector
				</button>
				<button
					class="flex-1 px-2 py-1.5 text-sm font-medium transition-colors border-b-2
						{rightTab === 'services'
						? 'border-primary text-foreground'
						: 'border-transparent text-muted-foreground hover:text-foreground'}"
					onclick={() => (rightTab = 'services')}
				>
					Services
				</button>
				<button
					class="flex-1 px-2 py-1.5 text-sm font-medium transition-colors border-b-2
						{rightTab === 'analysis'
						? 'border-primary text-foreground'
						: 'border-transparent text-muted-foreground hover:text-foreground'}"
					onclick={() => (rightTab = 'analysis')}
				>
					Analysis
				</button>
				<button
					class="flex-1 px-2 py-1.5 text-sm font-medium transition-colors border-b-2
						{rightTab === 'memory'
						? 'border-primary text-foreground'
						: 'border-transparent text-muted-foreground hover:text-foreground'}"
					onclick={() => (rightTab = 'memory')}
				>
					Memory
				</button>
			</div>

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
						onNavigateToChild={(id) => onSelectNet(id)}
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
				{:else if rightTab === 'memory'}
					<MemoryPanel
						memory={petriStore.memory}
						onRefresh={() => petriStore.fetchMemory()}
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
	{#if showScriptSheet && scriptTransition}
		<TransitionScriptSheet
			transition={scriptTransition.transition}
			inputPorts={scriptTransition.transition.input_ports ?? []}
			outputPorts={scriptTransition.transition.output_ports ?? []}
			guard={scriptTransition.transition.guard ?? null}
			script={scriptTransition.transition.script ?? ''}
			status={selectedElement?.type === 'transition'
				? petriStore.transitionStatuses[selectedElement.id]
				: undefined}
			effectHandlerId={scriptTransition.transition.effect_handler_id}
			open={showScriptSheet}
			onClose={() => (showScriptSheet = false)}
			onSaveScript={async (tid, script, guard) => {
				await petriStore.saveTransitionScript(tid, script, guard);
			}}
		/>
	{/if}

	{#if showTokenSheet && tokenSheetData}
		<TokenDetailSheet
			token={tokenSheetData.token}
			placeName={tokenSheetData.placeName}
			open={showTokenSheet}
			onClose={() => (showTokenSheet = false)}
			events={petriStore.events}
			getPlaceName={(id) => petriStore.getPlaceName(id)}
			getTransitionName={(id) => petriStore.getTransitionName(id)}
		/>
	{/if}

	{#if showScenarioEditor}
		<ScenarioEditor store={petriStore} {netId} onClose={() => (showScenarioEditor = false)} />
	{/if}
</div>
