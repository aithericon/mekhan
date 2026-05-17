<script lang="ts">
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { NetWorkbench, LabCanvas } from '$lib/components/petri';
	import type { WorkbenchApi } from '$lib/components/petri/NetWorkbench.svelte';
	import type { PetriNet, Token } from '$lib/types/petri';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import Play from '@lucide/svelte/icons/play';
	import Pause from '@lucide/svelte/icons/pause';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import FileText from '@lucide/svelte/icons/file-text';
	import PanelLeftClose from '@lucide/svelte/icons/panel-left-close';
	import PanelLeftOpen from '@lucide/svelte/icons/panel-left-open';
	import Layers from '@lucide/svelte/icons/layers';
	import X from '@lucide/svelte/icons/x';
	import {
		fetchCloudLayerTopology,
		fetchTokenPayload,
		subscribeToCloudLayerStream,
		markingProjectionToTokenMap
	} from '$lib/cloud-layer/index';
	import type { ArtifactPublishedEvent, TokenPayloadResponse, TransitionFiredEvent } from '$lib/cloud-layer/index';

	const PETRI_URL = '/petri';
	const netId = $derived($page.params.id as string);
	const owningInstanceId = $derived(
		netId.startsWith('mekhan-') ? netId.slice('mekhan-'.length) : null
	);

	// ── Cloud-layer mode detection ─────────────────────────────────────────
	const cloudLayerSource = $derived($page.url.searchParams.get('source'));
	const cloudLayerRunId = $derived($page.url.searchParams.get('run_id'));
	const isCloudLayerMode = $derived(
		cloudLayerSource === 'cloud-layer' && cloudLayerRunId !== null && cloudLayerRunId !== ''
	);

	// ── Cloud-layer state ──────────────────────────────────────────────────
	let clTopology = $state<PetriNet | null>(null);
	let clMarking = $state<Map<string, Token[]>>(new Map());
	let clError = $state<string | null>(null);
	let clLoading = $state(false);
	let clFiredTransitions = $state<TransitionFiredEvent[]>([]);
	let clArtifacts = $state<ArtifactPublishedEvent[]>([]);
	let clLastMarkingRaw = $state<Record<string, string[]>>({});

	// Token inspect popup state
	let clInspectedTokenId = $state<string | null>(null);
	let clInspectedTokenPlaceId = $state<string | null>(null);
	let clTokenPayload = $state<TokenPayloadResponse | null>(null);
	let clTokenPayloadLoading = $state(false);
	let clTokenPayloadError = $state<string | null>(null);

	// Side-panel tab (cloud-layer mode only)
	let clPanelTab = $state<'events' | 'artifacts'>('events');
	let clPanelOpen = $state(true);

	// ── Cloud-layer lifecycle ──────────────────────────────────────────────

	let unsubscribeStream: (() => void) | null = null;

	async function initCloudLayer(runId: string) {
		clLoading = true;
		clError = null;
		clTopology = null;
		clMarking = new Map();
		clFiredTransitions = [];
		clArtifacts = [];
		clLastMarkingRaw = {};
		clInspectedTokenId = null;
		clTokenPayload = null;

		try {
			clTopology = await fetchCloudLayerTopology(runId);
		} catch (e: unknown) {
			clError = e instanceof Error ? e.message : String(e);
			clLoading = false;
			return;
		} finally {
			clLoading = false;
		}

		unsubscribeStream = subscribeToCloudLayerStream(
			runId,
			(event) => {
				if (event.type === 'marking_updated') {
					clLastMarkingRaw = event.marking;
					clMarking = markingProjectionToTokenMap(event.marking);
				} else if (event.type === 'token_added') {
					// token_added is a hint; marking_updated drives the canonical marking.
					// Optimistically add a synthetic token to the current marking.
					const existing = clMarking.get(event.place_id) ?? [];
					const alreadyPresent = existing.some((t) => t.id === event.token_id);
					if (!alreadyPresent) {
						const newToken: Token = {
							id: event.token_id,
							color: { type: 'Unit' },
							created_at: new Date().toISOString()
						};
						const updated = new Map(clMarking);
						updated.set(event.place_id, [...existing, newToken]);
						clMarking = updated;
					}
				} else if (event.type === 'transition_fired') {
					clFiredTransitions = [event, ...clFiredTransitions].slice(0, 100);
				} else if (event.type === 'artifact_published') {
					clArtifacts = [event, ...clArtifacts].slice(0, 50);
				}
			},
			(_err) => {
				clError = 'SSE stream disconnected';
			}
		);
	}

	function destroyCloudLayer() {
		if (unsubscribeStream) {
			unsubscribeStream();
			unsubscribeStream = null;
		}
	}

	// Token inspect
	async function handleSelectToken(placeId: string, tokenId: string) {
		if (!cloudLayerRunId) return;
		clInspectedTokenId = tokenId;
		clInspectedTokenPlaceId = placeId;
		clTokenPayload = null;
		clTokenPayloadError = null;
		clTokenPayloadLoading = true;
		try {
			clTokenPayload = await fetchTokenPayload(cloudLayerRunId, tokenId);
		} catch (e: unknown) {
			clTokenPayloadError = e instanceof Error ? e.message : String(e);
		} finally {
			clTokenPayloadLoading = false;
		}
	}

	function closeTokenInspect() {
		clInspectedTokenId = null;
		clInspectedTokenPlaceId = null;
		clTokenPayload = null;
		clTokenPayloadError = null;
	}

	// ── Native mode helpers ─────────────────────────────────────────────────

	async function handleDeleteNet(id: string) {
		if (!confirm(`Delete net "${id}"?`)) return;
		try {
			await fetch(`${PETRI_URL}/api/nets/${id}`, { method: 'DELETE' });
			if (id === netId) goto('/nets');
		} catch {
			/* ignore */
		}
	}

	// ── Lifecycle: cloud-layer vs native ───────────────────────────────────

	onMount(() => {
		if (isCloudLayerMode && cloudLayerRunId) {
			initCloudLayer(cloudLayerRunId);
		}
		return () => {
			destroyCloudLayer();
		};
	});

	// Re-initialise when run_id query param changes (e.g. nav between runs)
	$effect(() => {
		const runId = cloudLayerRunId;
		const mode = isCloudLayerMode;
		if (mode && runId) {
			destroyCloudLayer();
			initCloudLayer(runId);
		} else if (!mode) {
			destroyCloudLayer();
		}
	});

	// ── Derived: transition name map from topology ─────────────────────────
	const clTransitionNames = $derived.by(() => {
		const m = new Map<string, string>();
		if (clTopology) {
			for (const t of clTopology.transitions) m.set(t.id, t.name);
		}
		return m;
	});

	const clPlaceNames = $derived.by(() => {
		const m = new Map<string, string>();
		if (clTopology) {
			for (const p of clTopology.places) m.set(p.id, p.name);
		}
		return m;
	});

	function clTransitionName(id: string): string {
		return clTransitionNames.get(id) ?? id;
	}
</script>

{#if isCloudLayerMode}
	<!-- ── Cloud-layer mode ───────────────────────────────────────────────── -->
	<div class="flex h-full flex-col bg-background" data-testid="cloud-layer-net-view">
		<!-- Header -->
		<div class="flex items-center gap-3 border-b border-border px-4 py-2 shrink-0">
			<Button variant="ghost" size="icon-sm" href="/nets">
				<ArrowLeft class="size-4" />
			</Button>
			<div class="flex items-center gap-2">
				<span class="font-mono text-sm font-medium" data-testid="cloud-layer-net-id">{netId}</span>
				<Badge class="bg-violet-100 text-violet-700 dark:bg-violet-900/30 dark:text-violet-400">
					cloud-layer
				</Badge>
				{#if cloudLayerRunId}
					<span class="text-xs text-muted-foreground font-mono" data-testid="cloud-layer-run-id">
						run: {cloudLayerRunId.slice(0, 8)}…
					</span>
				{/if}
			</div>
			<div class="ml-auto flex items-center gap-1">
				<Button
					variant="ghost"
					size="icon-sm"
					onclick={() => (clPanelOpen = !clPanelOpen)}
					title="Toggle event log"
					data-testid="cloud-layer-toggle-panel"
				>
					{#if clPanelOpen}
						<PanelLeftClose class="size-4" />
					{:else}
						<PanelLeftOpen class="size-4" />
					{/if}
				</Button>
			</div>
		</div>

		<!-- Main content -->
		<div class="flex flex-1 min-h-0">
			<!-- Canvas area -->
			<div class="flex flex-1 flex-col min-w-0 relative">
				{#if clLoading}
					<div class="flex items-center justify-center h-full text-sm text-muted-foreground">
						Loading cloud-layer topology…
					</div>
				{:else if clError}
					<div
						class="flex items-center justify-center h-full text-sm text-destructive"
						data-testid="cloud-layer-error"
					>
						{clError}
					</div>
				{:else if clTopology}
					<LabCanvas
						topology={clTopology}
						marking={clMarking}
						bridgedOutTokens={new Map()}
						enabledTransitions={[]}
						transitionStatuses={{}}
						groups={[]}
						onFireTransition={() => {}}
						onSelectToken={handleSelectToken}
					/>
				{:else}
					<div class="flex items-center justify-center h-full text-sm text-muted-foreground">
						No topology available
					</div>
				{/if}

				<!-- Token inspect popover (inline overlay) -->
				{#if clInspectedTokenId}
					<div
						class="absolute bottom-4 left-4 z-50 w-96 rounded-lg border border-border bg-background shadow-lg"
						data-testid="cloud-layer-token-inspect"
					>
						<div class="flex items-center justify-between border-b border-border px-3 py-2">
							<div class="flex items-center gap-2">
								<span class="text-xs font-medium">Token payload</span>
								{#if clInspectedTokenPlaceId}
									<span class="text-xs text-muted-foreground">
										@ {clPlaceNames.get(clInspectedTokenPlaceId) ?? clInspectedTokenPlaceId}
									</span>
								{/if}
							</div>
							<Button variant="ghost" size="icon-sm" onclick={closeTokenInspect}>
								<X class="size-3.5" />
							</Button>
						</div>
						<div class="p-3 max-h-64 overflow-auto" data-testid="cloud-layer-token-payload">
							{#if clTokenPayloadLoading}
								<span class="text-xs text-muted-foreground">Loading…</span>
							{:else if clTokenPayloadError}
								<span class="text-xs text-destructive">{clTokenPayloadError}</span>
							{:else if clTokenPayload}
								<div class="space-y-1">
									<div class="text-xs text-muted-foreground">
										color: <span class="font-mono">{clTokenPayload.token_color}</span>
									</div>
									<pre class="text-xs font-mono whitespace-pre-wrap break-all">{JSON.stringify(
											clTokenPayload.value,
											null,
											2
										)}</pre>
								</div>
							{/if}
						</div>
						<div class="border-t border-border px-3 py-1.5">
							<span class="font-mono text-xs text-muted-foreground break-all"
								>{clInspectedTokenId}</span
							>
						</div>
					</div>
				{/if}
			</div>

			<!-- Right panel: events + artifacts -->
			{#if clPanelOpen}
				<div
					class="w-80 border-l border-border shrink-0 flex flex-col"
					data-testid="cloud-layer-event-panel"
				>
					<!-- Tab bar -->
					<div class="flex border-b border-border shrink-0">
						<button
							class="flex-1 px-2 py-1.5 text-xs font-medium transition-colors border-b-2
								{clPanelTab === 'events'
								? 'border-primary text-foreground'
								: 'border-transparent text-muted-foreground hover:text-foreground'}"
							onclick={() => (clPanelTab = 'events')}
							data-testid="cloud-layer-events-tab"
						>
							Transitions
						</button>
						<button
							class="flex-1 px-2 py-1.5 text-xs font-medium transition-colors border-b-2
								{clPanelTab === 'artifacts'
								? 'border-primary text-foreground'
								: 'border-transparent text-muted-foreground hover:text-foreground'}"
							onclick={() => (clPanelTab = 'artifacts')}
							data-testid="cloud-layer-artifacts-tab"
						>
							Artifacts
							{#if clArtifacts.length > 0}
								<Badge class="ml-1 px-1 py-0 text-[10px]">{clArtifacts.length}</Badge>
							{/if}
						</button>
					</div>

					<!-- Tab content -->
					<div class="flex-1 min-h-0 overflow-y-auto">
						{#if clPanelTab === 'events'}
							{#if clFiredTransitions.length === 0}
								<div
									class="flex items-center justify-center h-32 text-xs text-muted-foreground"
								>
									No transitions yet
								</div>
							{:else}
								<ul class="divide-y divide-border">
									{#each clFiredTransitions as ev (ev.transition_id + (ev.outcome ?? ''))}
										<li
											class="px-3 py-2 text-xs space-y-0.5"
											data-testid="cloud-layer-transition-event"
										>
											<div class="flex items-center justify-between gap-2">
												<span class="font-medium truncate"
													>{clTransitionName(ev.transition_id)}</span
												>
												<Badge
													class={ev.outcome === 'completed'
														? 'bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400 text-[10px] px-1 py-0'
														: 'bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400 text-[10px] px-1 py-0'}
												>
													{ev.outcome}
												</Badge>
											</div>
											{#if ev.error_message}
												<div class="text-destructive truncate" title={ev.error_message}>
													{ev.error_message}
												</div>
											{/if}
											<div class="text-muted-foreground font-mono truncate">
												{ev.transition_id.slice(0, 8)}…
											</div>
										</li>
									{/each}
								</ul>
							{/if}
						{:else}
							{#if clArtifacts.length === 0}
								<div
									class="flex items-center justify-center h-32 text-xs text-muted-foreground"
								>
									No artifacts published
								</div>
							{:else}
								<ul class="divide-y divide-border">
									{#each clArtifacts as artifact}
										<li
											class="px-3 py-2 text-xs space-y-0.5"
											data-testid="cloud-layer-artifact-event"
										>
											<div class="flex items-center gap-2">
												<Layers class="size-3 shrink-0 text-violet-500" />
												<span class="font-medium truncate">
													{clTransitionName(artifact.transition_id)}
												</span>
											</div>
											{#if artifact.artifact.artifact_ref}
												<div class="text-muted-foreground font-mono truncate">
													{String(artifact.artifact.artifact_ref)}
												</div>
											{:else if artifact.artifact.artifact_url}
												<a
													href={String(artifact.artifact.artifact_url)}
													class="text-primary underline truncate block"
													target="_blank"
													rel="noopener noreferrer"
												>
													{String(artifact.artifact.artifact_url)}
												</a>
											{/if}
										</li>
									{/each}
								</ul>
							{/if}
						{/if}
					</div>
				</div>
			{/if}
		</div>
	</div>
{:else}
	<!-- ── Native mekhan-NATS mode (unchanged) ───────────────────────────── -->
	{#snippet header(api: WorkbenchApi)}
		<div class="flex items-center gap-3 border-b border-border px-4 py-2 shrink-0">
			<Button variant="ghost" size="icon-sm" href="/nets">
				<ArrowLeft class="size-4" />
			</Button>
			<Button variant="ghost" size="icon-sm" onclick={api.toggleNetTree}>
				{#if api.netTreeOpen}
					<PanelLeftClose class="size-4" />
				{:else}
					<PanelLeftOpen class="size-4" />
				{/if}
			</Button>
			<div class="flex items-center gap-2">
				<span class="font-mono text-sm font-medium">{netId}</span>
				{#if api.store.runMode}
					<Badge
						class={api.store.runMode === 'running'
							? 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400'
							: 'bg-gray-100 text-gray-700 dark:bg-gray-800 dark:text-gray-400'}
					>
						{api.store.runMode}
					</Badge>
				{/if}
				{#if owningInstanceId}
					<Button variant="ghost" size="sm" href="/instances/{owningInstanceId}">
						Instance ▸
					</Button>
				{/if}
			</div>
			<div class="ml-auto flex items-center gap-1">
				<Button variant="outline" size="sm" onclick={api.openScenario}>
					<FileText class="size-3.5" /> Scenario
				</Button>
				<Button variant="outline" size="sm" onclick={() => api.store.reset()}>
					<RotateCcw class="size-3.5" /> Reset
				</Button>
				<Button
					variant="outline"
					size="sm"
					onclick={() =>
						api.store.setRunMode(api.store.runMode === 'running' ? 'stopped' : 'running')}
				>
					{#if api.store.runMode === 'running'}
						<Pause class="size-3.5" /> Pause
					{:else}
						<Play class="size-3.5" /> Start
					{/if}
				</Button>
				<Button variant="outline" size="sm" onclick={() => api.store.evaluate()}>
					<RotateCcw class="size-3.5" /> Eval
				</Button>
				<Button variant="ghost" size="icon-sm" onclick={api.refreshNets}>
					<RefreshCw class="size-3.5" />
				</Button>
			</div>
		</div>
	{/snippet}

	<NetWorkbench {netId} onDeleteNet={handleDeleteNet} {header} />
{/if}
