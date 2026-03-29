<script lang="ts">
	import { page } from '$app/stores';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { LabCanvas } from '$lib/components/petri';
	import { createPetriStore } from '$lib/stores/petri.svelte';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import Play from '@lucide/svelte/icons/play';
	import Pause from '@lucide/svelte/icons/pause';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';

	const netId = $derived($page.params.id);
	const petriStore = $derived(createPetriStore(`/petri/api/nets/${netId}`));

	let activeTab = $state<'petri' | 'events'>('petri');

	$effect(() => {
		petriStore.start();
		return () => petriStore.stop();
	});

	async function toggleRunMode() {
		const current = petriStore.runMode;
		const next = current === 'running' ? 'stopped' : 'running';
		await fetch(`/petri/api/nets/${netId}/run-mode`, {
			method: 'PUT',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ mode: next })
		});
		petriStore.refresh();
	}

	async function evaluate() {
		await fetch(`/petri/api/nets/${netId}/command/evaluate`, {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({})
		});
		petriStore.refresh();
	}
</script>

<div class="flex h-full flex-col">
	<!-- Header -->
	<div class="flex items-center gap-3 border-b border-border px-4 py-3">
		<Button variant="ghost" size="icon-sm" href="/nets">
			<ArrowLeft class="size-4" />
		</Button>
		<div class="flex flex-col">
			<span class="font-mono text-sm font-medium">{netId}</span>
			{#if petriStore.runMode}
				<Badge
					class={petriStore.runMode === 'running'
						? 'bg-blue-100 text-blue-700'
						: 'bg-gray-100 text-gray-700'}
				>
					{petriStore.runMode}
				</Badge>
			{/if}
		</div>
		<div class="ml-auto flex items-center gap-1">
			<Button variant="outline" size="sm" onclick={toggleRunMode}>
				{#if petriStore.runMode === 'running'}
					<Pause class="size-3.5" /> Pause
				{:else}
					<Play class="size-3.5" /> Start
				{/if}
			</Button>
			<Button variant="outline" size="sm" onclick={evaluate}>
				<RotateCcw class="size-3.5" /> Evaluate
			</Button>
		</div>
	</div>

	<!-- Tabs -->
	<div class="flex border-b border-border px-4">
		<button
			class="border-b-2 px-3 py-2 text-sm transition-colors {activeTab === 'petri'
				? 'border-primary text-foreground'
				: 'border-transparent text-muted-foreground hover:text-foreground'}"
			onclick={() => (activeTab = 'petri')}
		>
			Petri Net
		</button>
		<button
			class="border-b-2 px-3 py-2 text-sm transition-colors {activeTab === 'events'
				? 'border-primary text-foreground'
				: 'border-transparent text-muted-foreground hover:text-foreground'}"
			onclick={() => (activeTab = 'events')}
		>
			Events ({petriStore.events?.length ?? 0})
		</button>
	</div>

	<!-- Content -->
	<div class="flex flex-1 min-h-0">
		{#if activeTab === 'petri'}
			<div class="flex-1 relative">
				{#if petriStore.topology}
					<LabCanvas
						topology={petriStore.topology}
						marking={petriStore.projectedMarking}
						bridgedOutTokens={petriStore.bridgedOutTokens}
						enabledTransitions={Object.entries(petriStore.transitionStatuses)
							.filter(([_, s]) => s === 'enabled')
							.map(([id]) => id)}
						transitionStatuses={petriStore.transitionStatuses}
						groups={petriStore.currentGroups}
						selectedElementId={null}
						spotlight={null}
						markingDiff={null}
						onFireTransition={(id) => petriStore?.fireTransition(id)}
						onSelectPlace={() => {}}
						onSelectTransition={() => {}}
						onSelectToken={() => {}}
						onSelectGroup={() => {}}
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
		{:else}
			<div class="flex-1 overflow-y-auto p-4">
				{#if petriStore.events && petriStore.events.length > 0}
					<div class="space-y-1">
						{#each petriStore.events as event, i}
							<div class="rounded border border-border px-3 py-2 text-xs font-mono">
								<span class="text-muted-foreground">#{event.sequence}</span>
								<span class="ml-2">{event.event?.type ?? JSON.stringify(event.event).slice(0, 80)}</span>
							</div>
						{/each}
					</div>
				{:else}
					<p class="text-sm text-muted-foreground">No events yet.</p>
				{/if}
			</div>
		{/if}
	</div>
</div>
