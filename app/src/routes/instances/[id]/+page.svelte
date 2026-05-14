<script lang="ts">
	import { page } from '$app/state';
	import { onDestroy } from 'svelte';
	import { getInstance, cancelInstance, type WorkflowInstance } from '$lib/api/client';
	import { createPetriStore, type PetriStore } from '$lib/stores/petri.svelte';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { LabCanvas } from '$lib/components/petri';
	import { Timeline } from '$lib/components/petri';
	import { EventLog } from '$lib/components/petri';
	import { Inspector } from '$lib/components/petri';
	import { TokenDetailSheet } from '$lib/components/petri';
	import { TransitionScriptSheet } from '$lib/components/petri';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import Layers from '@lucide/svelte/icons/layers';
	import Info from '@lucide/svelte/icons/info';

	const instanceId = $derived(page.params.id!);

	let instance = $state<WorkflowInstance | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let activeTab = $state<'petri' | 'details'>('petri');
	let petriStore = $state<PetriStore | null>(null);

	// Token/Script sheet state
	let tokenSheetOpen = $state(false);
	let scriptSheetOpen = $state(false);

	const statusColors: Record<string, string> = {
		created: 'bg-gray-100 text-gray-700',
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700',
		cancelled: 'bg-slate-100 text-slate-700'
	};

	const formatDate = (s: string | null) => (s ? new Date(s).toLocaleString() : '-');

	async function load() {
		loading = true;
		error = null;
		try {
			instance = await getInstance(instanceId);
			if (instance.net_id && instance.status !== 'created') {
				petriStore?.destroy();
				const store = createPetriStore(instance.net_id);
				await store.init();
				petriStore = store;
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load instance';
		} finally {
			loading = false;
		}
	}

	async function handleCancel() {
		if (!instance || !confirm('Cancel this instance?')) return;
		try {
			await cancelInstance(instance.id);
			instance = { ...instance, status: 'cancelled' };
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to cancel';
		}
	}

	$effect(() => {
		load();
	});

	onDestroy(() => {
		petriStore?.destroy();
	});
</script>

<div class="flex h-full flex-col" data-testid="instance-page">
	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
			Loading...
		</div>
	{:else if error}
		<div class="mx-6 mt-6 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{:else if instance}
		<!-- Header bar -->
		<div class="flex items-center justify-between border-b border-border px-4 py-2 bg-card shrink-0">
			<div class="flex items-center gap-3">
				<h1 class="text-sm font-semibold text-foreground">Instance</h1>
				<Badge class={statusColors[instance.status] ?? ''} variant="secondary">
					{instance.status}
				</Badge>
				<span class="font-mono text-[11px] text-muted-foreground">{instance.net_id}</span>
			</div>
			<div class="flex items-center gap-2">
				<!-- Tab switcher -->
				<div class="flex rounded-md border border-border">
					<button
						class="px-2.5 py-1 text-xs font-medium transition-colors {activeTab === 'petri' ? 'bg-primary text-primary-foreground' : 'hover:bg-accent'}"
						onclick={() => (activeTab = 'petri')}
					>
						<Layers class="inline-block size-3 mr-1" />
						Petri Net
					</button>
					<button
						class="px-2.5 py-1 text-xs font-medium transition-colors border-l border-border {activeTab === 'details' ? 'bg-primary text-primary-foreground' : 'hover:bg-accent'}"
						onclick={() => (activeTab = 'details')}
					>
						<Info class="inline-block size-3 mr-1" />
						Details
					</button>
				</div>
				{#if instance.status === 'running' || instance.status === 'created'}
					<Button
						variant="outline"
						size="sm"
						class="border-destructive/30 text-destructive hover:bg-destructive/10"
						onclick={handleCancel}
					>
						Cancel
					</Button>
				{/if}
			</div>
		</div>

		{#if activeTab === 'petri' && petriStore}
			<!-- Petri net visualization (full height) -->
			<div class="flex flex-1 min-h-0">
				<!-- Canvas + timeline -->
				<div class="flex flex-1 flex-col min-w-0">
					<div class="flex-1 relative">
						<LabCanvas
							topology={petriStore.topology}
							marking={petriStore.projectedMarking}
							bridgedOutTokens={petriStore.bridgedOutTokens}
							enabledTransitions={Object.entries(petriStore.transitionStatuses)
								.filter(([_, s]) => s === 'enabled')
								.map(([id]) => id)}
							transitionStatuses={petriStore.transitionStatuses}
							groups={petriStore.currentGroups}
							selectedElementId={petriStore.selectedElement?.type === 'place' || petriStore.selectedElement?.type === 'transition'
								? petriStore.selectedElement.id
								: null}
							spotlight={petriStore.eventSpotlight}
							markingDiff={petriStore.markingDiff}
							onFireTransition={(id) => petriStore?.fireTransition(id)}
							onSelectPlace={(id) => petriStore?.selectPlace(id)}
							onSelectTransition={(id) => petriStore?.selectTransition(id)}
							onSelectToken={(placeId, tokenId) => petriStore?.selectToken(placeId, tokenId)}
							onSelectGroup={(id) => petriStore?.selectGroup(id)}
						/>
					</div>
					{#if petriStore.events.length > 0}
						<Timeline
							events={petriStore.events}
							currentIndex={petriStore.replayIndex}
							onIndexChange={(i) => petriStore?.setReplayIndex(i)}
							evaluating={petriStore.evaluating}
							runMode={petriStore.runMode}
							onEvaluate={() => petriStore?.evaluate()}
							onToggleRunMode={() => petriStore?.setRunMode(petriStore.runMode === 'running' ? 'stopped' : 'running')}
							onHibernate={() => petriStore?.hibernate()}
						/>
					{/if}
				</div>

				<!-- Right panel: Event log + Inspector -->
				<div class="w-[320px] shrink-0 border-l border-border flex flex-col bg-card">
					<!-- Inspector (top half) -->
					<div class="flex-1 overflow-y-auto border-b border-border min-h-0">
						<Inspector
							selectedElement={petriStore.selectedElement}
							placeDetails={petriStore.getSelectedPlaceDetails()}
							transitionDetails={petriStore.getSelectedTransitionDetails()}
							tokenDetails={petriStore.getSelectedTokenDetails()}
							eventDetails={petriStore.getSelectedEventDetails()}
							groupDetails={petriStore.getSelectedGroupDetails()}
							getTransitionName={(id) => petriStore?.getTransitionName(id) ?? id}
							getPlaceName={(id) => petriStore?.getPlaceName(id) ?? id}
							onInjectToken={async (placeId, data) => {
								if (!petriStore) return { success: false, error: 'No store' };
								return petriStore.injectToken(placeId, data);
							}}
							onSelectEvent={(seq) => petriStore?.selectEvent(seq)}
							onSetReplayIndex={(i) => petriStore?.setReplayIndex(i)}
							onOpenScript={() => (scriptSheetOpen = true)}
							onViewToken={() => (tokenSheetOpen = true)}
						/>
					</div>
					<!-- Event log (bottom half) -->
					<div class="h-[300px] shrink-0 overflow-hidden">
						<EventLog
							events={petriStore.events}
							currentIndex={petriStore.replayIndex}
							onSelectEvent={(i) => petriStore?.setReplayIndex(i)}
							onInspectEvent={(seq) => petriStore?.selectEvent(seq)}
							getTransitionName={(id) => petriStore?.getTransitionName(id) ?? id}
							getPlaceName={(id) => petriStore?.getPlaceName(id) ?? id}
						/>
					</div>
				</div>
			</div>

			<!-- Sheets -->
			{#if petriStore.selectedElement?.type === 'token'}
				{@const details = petriStore.getSelectedTokenDetails()}
				{#if details}
					<TokenDetailSheet
						token={details.token}
						placeName={details.placeName}
						open={tokenSheetOpen}
						onClose={() => (tokenSheetOpen = false)}
						events={petriStore.events}
						getPlaceName={(id) => petriStore?.getPlaceName(id) ?? id}
						getTransitionName={(id) => petriStore?.getTransitionName(id) ?? id}
					/>
				{/if}
			{/if}

			{#if petriStore.selectedElement?.type === 'transition'}
				{@const details = petriStore.getSelectedTransitionDetails()}
				{#if details}
					<TransitionScriptSheet
						transition={details.transition}
						inputPorts={details.transition.input_ports ?? []}
						outputPorts={details.transition.output_ports ?? []}
						guard={details.transition.guard ?? null}
						script={details.transition.script ?? ''}
						status={petriStore.transitionStatuses[details.transition.id]}
						effectHandlerId={details.transition.effect_handler_id}
						open={scriptSheetOpen}
						onClose={() => (scriptSheetOpen = false)}
						onSaveScript={async (id, script, guard) => {
							await petriStore?.saveTransitionScript(id, script, guard);
						}}
					/>
				{/if}
			{/if}

		{:else if activeTab === 'petri' && !petriStore}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				{#if instance.status === 'created'}
					Instance has not started yet. No Petri net is available.
				{:else}
					Loading Petri net...
				{/if}
			</div>

		{:else}
			<!-- Details tab (original content) -->
			<div class="mx-auto max-w-3xl px-6 py-8 overflow-y-auto">
				<div class="space-y-4">
					<div class="rounded-lg border border-border bg-card">
						<div class="border-b border-border px-4 py-2.5">
							<span class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
								Details
							</span>
						</div>
						<dl class="divide-y divide-border">
							<div class="flex justify-between px-4 py-2.5">
								<dt class="text-xs text-muted-foreground">Instance ID</dt>
								<dd class="font-mono text-xs text-foreground">{instance.id}</dd>
							</div>
							<div class="flex justify-between px-4 py-2.5">
								<dt class="text-xs text-muted-foreground">Template</dt>
								<dd class="text-xs text-foreground">
									<a href="/templates/{instance.template_id}" class="text-primary underline">
										v{instance.template_version}
									</a>
								</dd>
							</div>
							<div class="flex justify-between px-4 py-2.5">
								<dt class="text-xs text-muted-foreground">Net ID</dt>
								<dd class="font-mono text-xs text-foreground">{instance.net_id}</dd>
							</div>
							<div class="flex justify-between px-4 py-2.5">
								<dt class="text-xs text-muted-foreground">Created</dt>
								<dd class="text-xs text-foreground">{formatDate(instance.created_at)}</dd>
							</div>
							<div class="flex justify-between px-4 py-2.5">
								<dt class="text-xs text-muted-foreground">Started</dt>
								<dd class="text-xs text-foreground">{formatDate(instance.started_at ?? null)}</dd>
							</div>
							<div class="flex justify-between px-4 py-2.5">
								<dt class="text-xs text-muted-foreground">Completed</dt>
								<dd class="text-xs text-foreground">{formatDate(instance.completed_at ?? null)}</dd>
							</div>
							{#if instance.current_step}
								<div class="flex justify-between px-4 py-2.5">
									<dt class="text-xs text-muted-foreground">Current Step</dt>
									<dd class="text-xs font-medium text-foreground">{instance.current_step}</dd>
								</div>
							{/if}
						</dl>
					</div>
				</div>
			</div>
		{/if}
	{/if}
</div>
