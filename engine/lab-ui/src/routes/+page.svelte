<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { toast } from 'svelte-sonner';
	import { multiNetStore } from '$lib/stores/multi-net.svelte';
	import LabCanvas from '$lib/components/LabCanvas.svelte';
	import Timeline from '$lib/components/Timeline.svelte';
	import EventLog from '$lib/components/EventLog.svelte';
	import ScenarioEditor from '$lib/components/ScenarioEditor.svelte';
	import Inspector from '$lib/components/Inspector.svelte';
	import AnalysisPanel from '$lib/components/AnalysisPanel.svelte';
	import ServicesPanel from '$lib/components/ServicesPanel.svelte';
	import TransitionScriptSheet from '$lib/components/TransitionScriptSheet.svelte';
	import TokenDetailSheet from '$lib/components/TokenDetailSheet.svelte';
	import NetTreeSidebar from '$lib/components/NetTreeSidebar.svelte';
	import { Toaster } from '$lib/components/ui/sonner';
	import { TooltipProvider } from '$lib/components/ui/tooltip';
	import { Loader2, X, Plus, ChevronLeft, ChevronRight, ChevronUp, ChevronDown, AlertCircle, AlertTriangle, CheckCircle, Maximize2, Minimize2, Sun, Moon, Blocks, Search } from '@lucide/svelte';
	import { toggleMode, mode } from 'mode-watcher';

	let showScenarioEditor = $state(false);
	let showScriptSheet = $state(false);
	let showTokenSheet = $state(false);
	let activePanel = $state<'services' | 'analysis' | null>(null);
	const showAnalysis = $derived(activePanel === 'analysis');
	const showServices = $derived(activePanel === 'services');
	let selectedTransitionId = $state<string | null>(null);
	let presentationMode = $state(false);
	let waking = $state(false);
	let hibernating = $state(false);

	// Active store derived from multi-net store
	const store = $derived(multiNetStore.activeStore);
	const activeNetHibernated = $derived(multiNetStore.activeNetId ? multiNetStore.isHibernated(multiNetStore.activeNetId) : false);

	// Get selected transition details
	const selectedTransitionDetails = $derived.by(() => {
		if (!selectedTransitionId || !store?.topology) return null;
		const transition = store.topology.transitions.find(t => t.id === selectedTransitionId);
		if (!transition) return null;

		const inputPorts = (transition as any).input_ports ?? [];
		const outputPorts = (transition as any).output_ports ?? [];
		const script = (transition as any).script ?? '';
		const guard = (transition as any).guard as string | null ?? null;
		const status = store.transitionStatuses[selectedTransitionId];
		const effectHandlerId = (transition as any).effect_handler_id as string | undefined ?? null;

		return { transition, inputPorts, outputPorts, script, guard, status, effectHandlerId };
	});

	function handleTransitionSelect(id: string) {
		store?.selectTransition(id);
		selectedTransitionId = id;
		activePanel = null;
	}

	function openScriptSheet() {
		if (selectedTransitionId) {
			showScriptSheet = true;
		}
	}

	function openTokenSheet() {
		showTokenSheet = true;
	}

	function handleTokenSelect(placeId: string, tokenId: string) {
		store?.selectToken(placeId, tokenId);
		showTokenSheet = true;
		activePanel = null;
	}

	const selectedTokenDetails = $derived(store?.getSelectedTokenDetails() ?? null);

	// Watch for errors and show toast
	$effect(() => {
		if (store?.error) {
			toast.error('Transition Failed', {
				description: store.error,
			});
		}
	});

	function togglePresentation() {
		if (!presentationMode) {
			presentationMode = true;
			document.documentElement.requestFullscreen?.();
		} else {
			presentationMode = false;
			if (document.fullscreenElement) document.exitFullscreen?.();
		}
	}

	// Sync presentation mode when browser exits fullscreen (e.g. user presses Escape)
	$effect(() => {
		function onFullscreenChange() {
			if (!document.fullscreenElement && presentationMode) {
				presentationMode = false;
			}
		}
		document.addEventListener('fullscreenchange', onFullscreenChange);
		return () => document.removeEventListener('fullscreenchange', onFullscreenChange);
	});

	/** Update ?net= query param to reflect the active tab. */
	function syncNetQueryParam() {
		const netId = multiNetStore.activeNetId;
		const url = new URL(window.location.href);
		if (netId && netId !== 'default') {
			url.searchParams.set('net', netId);
		} else {
			url.searchParams.delete('net');
		}
		if (url.href !== window.location.href) {
			history.replaceState(history.state, '', url);
		}
	}

	onMount(async () => {
		// Register keyboard shortcut listener (capture phase to beat SvelteFlow)
		document.addEventListener('keydown', handleKeydown, true);

		// Capture ?net= before any async work changes the active tab
		const netParam = new URLSearchParams(window.location.search).get('net');

		// Try to discover backend-managed nets first
		await multiNetStore.fetchNets();

		// Restore active tab from query param
		if (netParam && multiNetStore.nets.some((n) => n.netId === netParam)) {
			multiNetStore.setActive(netParam);
		}

		// Initialize whichever store is now active
		const s = multiNetStore.activeStore;
		if (s) {
			await s.fetchTopology();
			await s.fetchEvents();
			await s.fetchAnalysis();
			s.fetchServices();
			await s.fetchRunMode();
		}

		syncNetQueryParam();
	});

	onDestroy(() => {
		document.removeEventListener('keydown', handleKeydown, true);
	});

	// Switch to an adjacent tab and refresh its data
	async function switchTab(direction: -1 | 1) {
		const tabs = multiNetStore.nets;
		if (tabs.length <= 1) return;
		const idx = tabs.findIndex(t => t.netId === multiNetStore.activeNetId);
		const next = tabs[(idx + direction + tabs.length) % tabs.length];
		multiNetStore.setActive(next.netId);
		syncNetQueryParam();
		const s = multiNetStore.activeStore;
		if (s) {
			s.fetchTopology();
			await s.fetchEvents();
			s.fetchAnalysis();
			s.fetchServices();
			s.fetchRunMode();
			s.setReplayIndex(s.events.length - 1);
		}
	}

	/** Switch to a specific net by ID and refresh its data. Wakes hibernated nets automatically. */
	async function selectNet(netId: string) {
		// Ensure tab exists (addNet is idempotent)
		multiNetStore.addNet(netId);
		multiNetStore.setActive(netId);
		syncNetQueryParam();

		// Wake hibernated nets before fetching data
		if (multiNetStore.isHibernated(netId)) {
			waking = true;
			try {
				await multiNetStore.wakeNet(netId);
				await multiNetStore.fetchNets();
			} finally {
				waking = false;
			}
		}

		const s = multiNetStore.activeStore;
		if (s) {
			s.fetchTopology();
			await s.fetchEvents();
			s.fetchAnalysis();
			s.fetchServices();
			s.fetchRunMode();
			s.setReplayIndex(s.events.length - 1);
		}
	}

	/** Navigate to a child net from a RemoteNetNode. */
	async function navigateToChildNet(childNetId: string) {
		if (!childNetId) {
			toast.warning('No child net spawned yet');
			return;
		}
		await multiNetStore.fetchNets();
		await selectNet(childNetId);
	}

	function handleKeydown(e: KeyboardEvent) {
		const el = e.target as HTMLElement;
		if (el?.tagName === 'INPUT' || el?.tagName === 'TEXTAREA' || el?.tagName === 'SELECT') return;
		if (el?.isContentEditable) return;

		if (e.key === 'Escape' && presentationMode) {
			e.preventDefault();
			togglePresentation();
			return;
		}

		if (!(e.metaKey || e.ctrlKey)) return;
		if (e.key === 'h') { e.preventDefault(); switchTab(-1); }
		if (e.key === 'l') { e.preventDefault(); switchTab(1); }
	}

	// Derive selected node ID for canvas highlight ring
	const selectedNodeId = $derived.by(() => {
		const elem = store?.selectedElement;
		if (!elem) return null;
		if (elem.type === 'place') return elem.id;
		if (elem.type === 'transition') return elem.id;
		if (elem.type === 'group') return elem.id;
		if (elem.type === 'remotenet') return elem.id;
		return null;
	});

	// Analysis summary for the toggle button indicator
	const analysisSummary = $derived(store?.analysisReport?.summary ?? null);
	const analysisValid = $derived(store?.analysisReport?.is_valid ?? true);
	const issueCount = $derived(
		(analysisSummary?.error_count ?? 0) + (analysisSummary?.warning_count ?? 0)
	);

	const serviceCount = $derived(store?.services?.handlers?.length ?? 0);

	// Get enabled transitions from backend's transition statuses (includes guard evaluation)
	const enabledTransitions = $derived.by(() => {
		if (!store) return [];
		const statuses = store.transitionStatuses;
		const enabled: string[] = [];

		for (const [transitionId, status] of Object.entries(statuses)) {
			if (status.status === 'enabled') {
				enabled.push(transitionId);
			}
		}

		return enabled;
	});
</script>


<svelte:head>
	<title>Aithericon Lab</title>
</svelte:head>

<TooltipProvider>
<div id="app" class="app h-screen flex flex-col bg-background">
	<!-- Header -->
	<header id="main-header" class="bg-navigation text-foreground px-4 py-3 flex items-center justify-between" class:hidden={presentationMode}>
		<div class="flex items-center gap-3">
			<img src="/Logo+Name-white.svg" alt="Aithericon" class="h-7" />
			<span class="text-sm text-muted-foreground">Petri Net Lab</span>
		</div>

		<div id="header-actions" class="flex items-center gap-2">
			<!-- Fixed-size spinner container - always takes space to prevent layout shift -->
			<div class="w-4 h-4 flex items-center justify-center">
				{#if store?.loading}
					<Loader2 class="w-4 h-4 text-muted-foreground animate-spin" />
				{/if}
			</div>
			{#if store?.error}
				<span id="error-message" class="text-sm text-red-400">{store.error}</span>
			{/if}
			<button
				class="p-1.5 rounded text-muted-foreground hover:text-foreground hover:bg-secondary"
				onclick={toggleMode}
				title="Toggle theme"
			>
				{#if mode.current === 'dark'}
					<Sun class="w-4 h-4" />
				{:else}
					<Moon class="w-4 h-4" />
				{/if}
			</button>
			<button
				class="p-1.5 rounded text-muted-foreground hover:text-foreground hover:bg-secondary"
				onclick={togglePresentation}
				title="Presentation mode"
			>
				<Maximize2 class="w-4 h-4" />
			</button>
			<button
				id="btn-load-scenario"
				class="px-3 py-1 text-sm bg-green-600 hover:bg-green-500 rounded"
				onclick={() => (showScenarioEditor = true)}
			>
				Load Scenario
			</button>
			<button
				id="btn-reset"
				class="px-3 py-1 text-sm bg-secondary text-secondary-foreground hover:bg-accent rounded"
				onclick={() => store?.reset()}
			>
				Reset
			</button>
			<button
				id="btn-refresh"
				class="px-3 py-1 text-sm bg-primary text-primary-foreground hover:bg-primary/90 rounded"
				onclick={async () => {
					if (!store) return;
					await store.fetchTopology();
					await store.fetchEvents();
					await store.fetchAnalysis();
					store.fetchServices();
				}}
			>
				Refresh
			</button>
		</div>
	</header>

	<!-- Main content -->
	<div id="main-content" class="flex-1 flex overflow-hidden">
		<!-- Net tree sidebar -->
		<div id="net-tree-sidebar" class="w-56 flex-shrink-0 border-r border-border" class:hidden={presentationMode}>
			<NetTreeSidebar
				tree={multiNetStore.tree}
				activeNetId={multiNetStore.activeNetId}
				wakingNetId={waking ? multiNetStore.activeNetId : undefined}
				onSelectNet={selectNet}
				onRemoveNet={async (netId) => {
					await multiNetStore.removeNet(netId);
					await multiNetStore.fetchNets();
				}}
				onRefresh={() => multiNetStore.fetchNets()}
			statusFilter={multiNetStore.statusFilter}
			onToggleFilter={() => {
				multiNetStore.setStatusFilter(multiNetStore.statusFilter === 'active' ? 'all' : 'active');
				multiNetStore.fetchNets();
			}}
			/>
		</div>

		<!-- Canvas area -->
	{#if store && (!activeNetHibernated || hibernating)}
		<div id="canvas-area" class="relative flex-1 flex flex-col min-h-0">
			<div class="flex-1 min-h-0">
				<LabCanvas
					{presentationMode}
					topology={store.topology}
					marking={store.projectedMarking}
					bridgedOutTokens={store.bridgedOutTokens}
					{enabledTransitions}
					transitionStatuses={store.transitionStatuses}
					issues={store.analysisReport?.issues ?? []}
					groups={store.groups}
					selectedElementId={selectedNodeId}
					spotlight={store.eventSpotlight}
					markingDiff={store.markingDiff}
					netId={multiNetStore.activeNetId}
					spawnChildren={multiNetStore.spawnChildren}
					onNavigateToChild={navigateToChildNet}
					onFireTransition={(id) => store.fireTransition(id)}
					onSelectPlace={(id) => { store.selectPlace(id); activePanel = null; }}
					onSelectTransition={handleTransitionSelect}
					onSelectToken={handleTokenSelect}
					onSelectGroup={(id) => { store.selectGroup(id); activePanel = null; }}
					onSelectRemoteNet={(id, label, targets, sources, childNetIds) => { store.selectRemoteNet(id, label, targets, sources, childNetIds); activePanel = null; }}
				/>
			</div>

			<!-- Wake overlay -->
			{#if waking}
				<div class="absolute inset-0 z-20 flex flex-col items-center justify-center bg-background/80 backdrop-blur-sm animate-in fade-in-0 duration-200">
					<Loader2 class="w-8 h-8 text-primary animate-spin" />
					<p class="mt-3 text-sm font-medium text-foreground">Waking net...</p>
					<p class="mt-1 text-xs text-muted-foreground">Replaying events from NATS</p>
				</div>
			{/if}

			<!-- Hibernate overlay -->
			{#if hibernating}
				<div class="absolute inset-0 z-20 flex flex-col items-center justify-center bg-background/80 backdrop-blur-sm animate-in fade-in-0 duration-200">
					<Moon class="w-8 h-8 text-muted-foreground animate-pulse" />
					<p class="mt-3 text-sm font-medium text-foreground">Hibernating...</p>
					<p class="mt-1 text-xs text-muted-foreground">Preserving events to NATS</p>
				</div>
			{/if}

			<!-- Timeline -->
			{#if store.events.length > 0 && !presentationMode}
				<Timeline
					events={store.events}
					currentIndex={store.replayIndex}
					onIndexChange={(idx) => store.setReplayIndex(idx)}
					evaluating={store.evaluating}
					runMode={store.runMode}
					onEvaluate={async () => {
						const result = await store.evaluate();
						if (result.success) {
							toast.success('Evaluation complete', {
								description: `Fired ${result.stepsExecuted} transitions (${result.finalState})`
							});
						}
					}}
					onToggleRunMode={async () => {
						const newMode = store.runMode === 'running' ? 'stopped' : 'running';
						await store.setRunMode(newMode);
					}}
					onHibernate={async () => {
						hibernating = true;
						try {
							const result = await store.hibernate();
							if (result.success) {
								toast.success('Net hibernated', {
									description: `${store.netId} has been hibernated. Events are preserved in NATS.`
								});
								store.stopLiveUpdates();
								await multiNetStore.fetchNets();
							} else {
								toast.error('Hibernate failed', { description: result.error });
							}
						} finally {
							hibernating = false;
						}
					}}
				/>
			{/if}
		</div>

		<!-- Sidebar panels -->
		<div id="inspector-sidebar" class="w-80 flex-shrink-0 flex flex-col h-full" class:hidden={presentationMode}>
			<!-- Panel selector -->
			<div class="shrink-0 flex items-center border-b border-border bg-muted">
				<button
					class="flex-1 flex items-center justify-center gap-1.5 px-3 py-2 text-xs font-medium transition-colors {activePanel === null ? 'text-foreground bg-accent/50' : 'text-muted-foreground hover:text-foreground hover:bg-accent/30'}"
					onclick={() => (activePanel = null)}
				>
					<Search class="w-3.5 h-3.5" />
					Inspector
				</button>
				<button
					class="flex-1 flex items-center justify-center gap-1.5 px-3 py-2 text-xs font-medium transition-colors {activePanel === 'services' ? 'text-foreground bg-accent/50' : 'text-muted-foreground hover:text-foreground hover:bg-accent/30'}"
					onclick={() => (activePanel = 'services')}
				>
					<Blocks class="w-3.5 h-3.5" />
					Handlers
					{#if serviceCount > 0}
						<span class="text-[10px] font-medium px-1.5 py-0.5 rounded-full bg-purple-500/15 text-purple-500">
							{serviceCount}
						</span>
					{/if}
				</button>
				<button
					class="flex-1 flex items-center justify-center gap-1.5 px-3 py-2 text-xs font-medium transition-colors {activePanel === 'analysis' ? 'text-foreground bg-accent/50' : 'text-muted-foreground hover:text-foreground hover:bg-accent/30'}"
					onclick={() => (activePanel = 'analysis')}
				>
					{#if !analysisValid}
						<AlertCircle class="w-3.5 h-3.5 text-red-500" />
					{:else}
						<CheckCircle class="w-3.5 h-3.5 text-green-500" />
					{/if}
					Analysis
					{#if issueCount > 0}
						<span class="text-[10px] font-medium px-1.5 py-0.5 rounded-full bg-red-500/15 text-red-500">
							{issueCount}
						</span>
					{/if}
				</button>
			</div>

			<!-- Active panel content -->
			<div class="flex-1 min-h-0">
				{#if activePanel === 'services'}
					<ServicesPanel />
				{:else if activePanel === 'analysis'}
					<AnalysisPanel />
				{:else}
					<Inspector onOpenScript={openScriptSheet} onViewToken={openTokenSheet} onNavigateToChild={navigateToChildNet} />
				{/if}
			</div>
		</div>

		<!-- Event log sidebar -->
		<div id="event-log-sidebar" class="w-72 flex-shrink-0" class:hidden={presentationMode}>
			<EventLog
				events={store.events}
				currentIndex={store.replayIndex}
				onSelectEvent={(idx) => store.setReplayIndex(idx)}
				onInspectEvent={(seq) => store.selectEvent(seq)}
			/>
		</div>
	{:else if store && activeNetHibernated}
		<!-- Hibernated net placeholder -->
		<div class="flex-1 flex flex-col items-center justify-center bg-muted/20">
			<Moon class="w-12 h-12 text-muted-foreground/40" />
			<p class="mt-4 text-sm font-medium text-muted-foreground">Net is hibernated</p>
			<p class="mt-1 text-xs text-muted-foreground/70">Click to wake and resume editing</p>
			<button
				class="mt-4 px-4 py-2 text-xs font-medium rounded-md bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
				onclick={() => selectNet(multiNetStore.activeNetId)}
			>
				Wake net
			</button>
		</div>
	{/if}
	</div>
</div>
</TooltipProvider>

<!-- Presentation mode exit button -->
{#if presentationMode}
	<button
		class="fixed top-4 right-4 z-50 p-2 rounded-lg bg-black/20 text-white/30 hover:text-white hover:bg-black/60 backdrop-blur-sm transition-all"
		onclick={togglePresentation}
		title="Exit presentation (Esc)"
	>
		<Minimize2 class="w-5 h-5" />
	</button>
{/if}

<!-- Scenario Editor Modal -->
{#if showScenarioEditor}
	<ScenarioEditor onClose={() => (showScenarioEditor = false)} />
{/if}

<!-- Transition Script Sheet -->
{#if selectedTransitionDetails}
	<TransitionScriptSheet
		transition={selectedTransitionDetails.transition}
		inputPorts={selectedTransitionDetails.inputPorts}
		outputPorts={selectedTransitionDetails.outputPorts}
		guard={selectedTransitionDetails.guard}
		script={selectedTransitionDetails.script}
		status={selectedTransitionDetails.status}
		effectHandlerId={selectedTransitionDetails.effectHandlerId}
		open={showScriptSheet}
		onClose={() => (showScriptSheet = false)}
	/>
{/if}

<!-- Token Detail Sheet -->
{#if selectedTokenDetails}
	<TokenDetailSheet
		token={selectedTokenDetails.token}
		placeName={selectedTokenDetails.placeName}
		open={showTokenSheet}
		onClose={() => (showTokenSheet = false)}
	/>
{/if}

<!-- Sonner Toast Provider -->
<Toaster richColors position="bottom-center" />
