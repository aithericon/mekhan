<script lang="ts">
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { LabCanvas } from '$lib/components/petri';
	import type { PetriNet, Token } from '$lib/types/petri';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
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
	import type {
		ArtifactPublishedEvent,
		TokenPayloadResponse,
		TransitionFiredEvent
	} from '$lib/cloud-layer/index';

	// Route: `/runs/[run_id]` — cloud-layer-driven AI-pipeline run viewer.
	// Per sub-phase 2.4 Q-D corrected disposition (user, 2026-05-17):
	// this is a dedicated route for cloud-layer pipeline runs; NOT a
	// "cloud-layer mode" of `nets/[id]`. The two routes coexist as
	// independent surfaces: `nets/[id]` for mekhan-NATS-subscribed
	// mekhan-SOP-OS workflows; `runs/[run_id]` for cloud-layer AI pipelines.
	// Mode-discriminator framing was rejected per
	// `feedback_no_mode_framing_for_the_direction` (cleanup-attractor risk
	// post-2.5 clinic-pipeline_engine removal).
	const runId = $derived($page.params.run_id as string);

	// ── State ──────────────────────────────────────────────────────────────
	let topology = $state<PetriNet | null>(null);
	let marking = $state<Map<string, Token[]>>(new Map());
	let firedTransitions = $state<TransitionFiredEvent[]>([]);
	let artifacts = $state<ArtifactPublishedEvent[]>([]);
	let error = $state<string | null>(null);
	let loading = $state(false);
	let lastMarkingRaw = $state<Record<string, string[]>>({});

	// Token inspect popup state
	let inspectedTokenId = $state<string | null>(null);
	let inspectedTokenPlaceId = $state<string | null>(null);
	let tokenPayload = $state<TokenPayloadResponse | null>(null);
	let tokenPayloadLoading = $state(false);
	let tokenPayloadError = $state<string | null>(null);

	// Side-panel state
	let panelTab = $state<'events' | 'artifacts'>('events');
	let panelOpen = $state(true);

	// ── Lifecycle ──────────────────────────────────────────────────────────
	let unsubscribeStream: (() => void) | null = null;

	async function initRunView(rid: string) {
		loading = true;
		error = null;
		topology = null;
		marking = new Map();
		firedTransitions = [];
		artifacts = [];
		lastMarkingRaw = {};
		inspectedTokenId = null;
		tokenPayload = null;

		try {
			topology = await fetchCloudLayerTopology(rid);
		} catch (e: unknown) {
			error = e instanceof Error ? e.message : String(e);
			loading = false;
			return;
		} finally {
			loading = false;
		}

		unsubscribeStream = subscribeToCloudLayerStream(
			rid,
			(event) => {
				if (event.type === 'marking_updated') {
					lastMarkingRaw = event.marking;
					marking = markingProjectionToTokenMap(event.marking);
				} else if (event.type === 'token_added') {
					// token_added is a hint; marking_updated drives the canonical marking.
					// Optimistically add a synthetic token to the current marking.
					const existing = marking.get(event.place_id) ?? [];
					const alreadyPresent = existing.some((t) => t.id === event.token_id);
					if (!alreadyPresent) {
						const newToken: Token = {
							id: event.token_id,
							color: { type: 'Unit' },
							created_at: new Date().toISOString()
						};
						const updated = new Map(marking);
						updated.set(event.place_id, [...existing, newToken]);
						marking = updated;
					}
				} else if (event.type === 'transition_fired') {
					firedTransitions = [event, ...firedTransitions].slice(0, 100);
				} else if (event.type === 'artifact_published') {
					artifacts = [event, ...artifacts].slice(0, 50);
				}
			},
			(_err) => {
				error = 'SSE stream disconnected';
			}
		);
	}

	function destroyRunView() {
		if (unsubscribeStream) {
			unsubscribeStream();
			unsubscribeStream = null;
		}
	}

	async function handleSelectToken(placeId: string, tokenId: string) {
		if (!runId) return;
		inspectedTokenId = tokenId;
		inspectedTokenPlaceId = placeId;
		tokenPayload = null;
		tokenPayloadError = null;
		tokenPayloadLoading = true;
		try {
			tokenPayload = await fetchTokenPayload(runId, tokenId);
		} catch (e: unknown) {
			tokenPayloadError = e instanceof Error ? e.message : String(e);
		} finally {
			tokenPayloadLoading = false;
		}
	}

	function closeTokenInspect() {
		inspectedTokenId = null;
		inspectedTokenPlaceId = null;
		tokenPayload = null;
		tokenPayloadError = null;
	}

	onMount(() => {
		if (runId) {
			initRunView(runId);
		}
		return () => {
			destroyRunView();
		};
	});

	// Re-initialise when run_id route param changes (e.g. nav between runs)
	$effect(() => {
		const rid = runId;
		if (rid) {
			destroyRunView();
			initRunView(rid);
		}
	});

	// ── Derived: name maps from topology ───────────────────────────────────
	const transitionNames = $derived.by(() => {
		const m = new Map<string, string>();
		if (topology) {
			for (const t of topology.transitions) m.set(t.id, t.name);
		}
		return m;
	});

	const placeNames = $derived.by(() => {
		const m = new Map<string, string>();
		if (topology) {
			for (const p of topology.places) m.set(p.id, p.name);
		}
		return m;
	});

	function transitionName(id: string): string {
		return transitionNames.get(id) ?? id;
	}
</script>

<div class="flex h-full flex-col bg-background" data-testid="cloud-layer-run-view">
	<!-- Header -->
	<div class="flex shrink-0 items-center gap-3 border-b border-border px-4 py-2">
		<Button variant="ghost" size="icon-sm" href="/">
			<ArrowLeft class="size-4" />
		</Button>
		<div class="flex items-center gap-2">
			<Badge class="bg-violet-100 text-violet-700 dark:bg-violet-900/30 dark:text-violet-400">
				cloud-layer
			</Badge>
			{#if runId}
				<span class="font-mono text-xs text-muted-foreground" data-testid="cloud-layer-run-id">
					run: {runId.slice(0, 8)}…
				</span>
			{/if}
		</div>
		<div class="ml-auto flex items-center gap-1">
			<Button
				variant="ghost"
				size="icon-sm"
				onclick={() => (panelOpen = !panelOpen)}
				title="Toggle event log"
				data-testid="cloud-layer-toggle-panel"
			>
				{#if panelOpen}
					<PanelLeftClose class="size-4" />
				{:else}
					<PanelLeftOpen class="size-4" />
				{/if}
			</Button>
		</div>
	</div>

	<!-- Main content -->
	<div class="flex min-h-0 flex-1">
		<!-- Canvas area -->
		<div class="relative flex min-w-0 flex-1 flex-col">
			{#if loading}
				<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
					Loading cloud-layer topology…
				</div>
			{:else if error}
				<div
					class="flex h-full items-center justify-center text-sm text-destructive"
					data-testid="cloud-layer-error"
				>
					{error}
				</div>
			{:else if topology}
				<LabCanvas
					{topology}
					{marking}
					bridgedOutTokens={new Map()}
					enabledTransitions={[]}
					transitionStatuses={{}}
					groups={[]}
					onFireTransition={() => {}}
					onSelectToken={handleSelectToken}
				/>
			{:else}
				<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
					No topology available
				</div>
			{/if}

			<!-- Token inspect popover (inline overlay) -->
			{#if inspectedTokenId}
				<div
					class="absolute bottom-4 left-4 z-50 w-96 rounded-lg border border-border bg-background shadow-lg"
					data-testid="cloud-layer-token-inspect"
				>
					<div class="flex items-center justify-between border-b border-border px-3 py-2">
						<div class="flex items-center gap-2">
							<span class="text-xs font-medium">Token payload</span>
							{#if inspectedTokenPlaceId}
								<span class="text-xs text-muted-foreground">
									@ {placeNames.get(inspectedTokenPlaceId) ?? inspectedTokenPlaceId}
								</span>
							{/if}
						</div>
						<Button variant="ghost" size="icon-sm" onclick={closeTokenInspect}>
							<X class="size-3.5" />
						</Button>
					</div>
					<div class="max-h-64 overflow-auto p-3" data-testid="cloud-layer-token-payload">
						{#if tokenPayloadLoading}
							<span class="text-xs text-muted-foreground">Loading…</span>
						{:else if tokenPayloadError}
							<span class="text-xs text-destructive">{tokenPayloadError}</span>
						{:else if tokenPayload}
							<div class="space-y-1">
								<div class="text-xs text-muted-foreground">
									color: <span class="font-mono">{tokenPayload.token_color}</span>
								</div>
								<pre
									class="whitespace-pre-wrap break-all font-mono text-xs">{JSON.stringify(
										tokenPayload.value,
										null,
										2
									)}</pre>
							</div>
						{/if}
					</div>
					<div class="border-t border-border px-3 py-1.5">
						<span class="break-all font-mono text-xs text-muted-foreground">{inspectedTokenId}</span>
					</div>
				</div>
			{/if}
		</div>

		<!-- Right panel: events + artifacts -->
		{#if panelOpen}
			<div
				class="flex w-80 shrink-0 flex-col border-l border-border"
				data-testid="cloud-layer-event-panel"
			>
				<!-- Tab bar -->
				<div class="flex shrink-0 border-b border-border">
					<button
						class="flex-1 border-b-2 px-2 py-1.5 text-xs font-medium transition-colors
							{panelTab === 'events'
							? 'border-primary text-foreground'
							: 'border-transparent text-muted-foreground hover:text-foreground'}"
						onclick={() => (panelTab = 'events')}
						data-testid="cloud-layer-events-tab"
					>
						Transitions
					</button>
					<button
						class="flex-1 border-b-2 px-2 py-1.5 text-xs font-medium transition-colors
							{panelTab === 'artifacts'
							? 'border-primary text-foreground'
							: 'border-transparent text-muted-foreground hover:text-foreground'}"
						onclick={() => (panelTab = 'artifacts')}
						data-testid="cloud-layer-artifacts-tab"
					>
						Artifacts
						{#if artifacts.length > 0}
							<Badge class="ml-1 px-1 py-0 text-[10px]">{artifacts.length}</Badge>
						{/if}
					</button>
				</div>

				<!-- Tab content -->
				<div class="min-h-0 flex-1 overflow-y-auto">
					{#if panelTab === 'events'}
						{#if firedTransitions.length === 0}
							<div class="flex h-32 items-center justify-center text-xs text-muted-foreground">
								No transitions yet
							</div>
						{:else}
							<ul class="divide-y divide-border">
								{#each firedTransitions as ev (ev.transition_id + (ev.outcome ?? ''))}
									<li
										class="space-y-0.5 px-3 py-2 text-xs"
										data-testid="cloud-layer-transition-event"
									>
										<div class="flex items-center justify-between gap-2">
											<span class="truncate font-medium">{transitionName(ev.transition_id)}</span>
											<Badge
												class={ev.outcome === 'completed'
													? 'bg-green-100 px-1 py-0 text-[10px] text-green-700 dark:bg-green-900/30 dark:text-green-400'
													: 'bg-red-100 px-1 py-0 text-[10px] text-red-700 dark:bg-red-900/30 dark:text-red-400'}
											>
												{ev.outcome}
											</Badge>
										</div>
										{#if ev.error_message}
											<div class="truncate text-destructive" title={ev.error_message}>
												{ev.error_message}
											</div>
										{/if}
										<div class="truncate font-mono text-muted-foreground">
											{ev.transition_id.slice(0, 8)}…
										</div>
									</li>
								{/each}
							</ul>
						{/if}
					{:else if artifacts.length === 0}
						<div class="flex h-32 items-center justify-center text-xs text-muted-foreground">
							No artifacts published
						</div>
					{:else}
						<ul class="divide-y divide-border">
							{#each artifacts as artifact}
								<li
									class="space-y-0.5 px-3 py-2 text-xs"
									data-testid="cloud-layer-artifact-event"
								>
									<div class="flex items-center gap-2">
										<Layers class="size-3 shrink-0 text-violet-500" />
										<span class="truncate font-medium">
											{transitionName(artifact.transition_id)}
										</span>
									</div>
									{#if artifact.artifact.artifact_ref}
										<div class="truncate font-mono text-muted-foreground">
											{String(artifact.artifact.artifact_ref)}
										</div>
									{:else if artifact.artifact.artifact_url}
										<a
											href={String(artifact.artifact.artifact_url)}
											class="block truncate text-primary underline"
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
				</div>
			</div>
		{/if}
	</div>
</div>
