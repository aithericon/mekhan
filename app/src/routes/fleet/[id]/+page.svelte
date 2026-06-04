<script lang="ts">
	// Per-group runner detail. Resolves the presence `capacity` by its id (the
	// CapacityCard "Detail" / "Enroll here" deep-link `/fleet/{id}`), then renders a
	// [Board | Roster | Interfaces] sub-tab layout SCOPED to that group — reusing the
	// shared fleet components with their new `group` filter prop so a per-group view
	// and the cross-group Control Plane share one implementation.
	import { page } from '$app/state';
	import * as Tabs from '$lib/components/ui/tabs';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import UserPlus from '@lucide/svelte/icons/user-plus';
	import PresenceBoard from '$lib/components/fleet/PresenceBoard.svelte';
	import RunnerList from '$lib/components/fleet/RunnerList.svelte';
	import InterfacesCatalog from '$lib/components/fleet/InterfacesCatalog.svelte';
	import EnrollSheet from '$lib/components/fleet/EnrollSheet.svelte';
	import { listCapacities, type CapacitySummary } from '$lib/api/capacities';

	const resourceId = $derived(page.params.id ?? '');

	let capacity = $state<CapacitySummary | null>(null);
	let error = $state<string | null>(null);
	let activeTab = $state<'board' | 'roster' | 'interfaces'>('board');
	let enrollOpen = $state(false);

	/** The group alias (capacity `path`) every child view is scoped to. */
	const groupAlias = $derived(capacity?.path ?? null);
	const name = $derived(capacity?.display_name || capacity?.path || resourceId);

	async function load() {
		try {
			const all = await listCapacities();
			capacity = all.find((c) => c.id === resourceId) ?? null;
			error = capacity ? null : 'Capacity not found.';
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load capacity';
		}
	}

	$effect(() => {
		void resourceId;
		void load();
	});
</script>

<svelte:head><title>{name} | Control Plane | Mekhan</title></svelte:head>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-6xl px-6 py-8 animate-rise">
		<a
			href="/fleet"
			class="mb-3 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
		>
			<ChevronLeft class="size-4" /> Control Plane
		</a>

		<div class="mb-4 flex items-start justify-between gap-3">
			<div>
				<h1 class="text-lg font-semibold tracking-tight" data-testid="group-detail-title">{name}</h1>
				<div class="mt-1 flex items-center gap-2 text-sm">
					<Badge variant="secondary">presence</Badge>
					{#if capacity}
						<span class="font-mono text-xs text-muted-foreground">{capacity.path}</span>
						{#if capacity.live.kind === 'presence'}
							<span class="text-xs text-muted-foreground tabular-nums">
								{capacity.live.online}/{capacity.live.total} online
							</span>
						{/if}
					{:else}
						<span class="font-mono text-xs text-muted-foreground">{resourceId}</span>
					{/if}
				</div>
			</div>
			{#if groupAlias}
				<Button
					variant="outline"
					size="sm"
					class="gap-1.5"
					onclick={() => (enrollOpen = true)}
					data-testid="group-enroll-here"
				>
					<UserPlus class="size-4" />
					Enroll here
				</Button>
			{/if}
		</div>

		{#if error}
			<div
				class="mb-3 rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive"
			>
				{error}
			</div>
		{/if}

		<Tabs.Root value={activeTab} onValueChange={(v) => (activeTab = v as typeof activeTab)} class="mb-5">
			<Tabs.List>
				<Tabs.Trigger value="board" data-testid="group-tab-board">Board</Tabs.Trigger>
				<Tabs.Trigger value="roster" data-testid="group-tab-roster">Roster</Tabs.Trigger>
				<Tabs.Trigger value="interfaces" data-testid="group-tab-interfaces">Interfaces</Tabs.Trigger>
			</Tabs.List>
		</Tabs.Root>

		{#if groupAlias}
			{#if activeTab === 'board'}
				<PresenceBoard group={groupAlias} />
			{:else if activeTab === 'roster'}
				<RunnerList group={groupAlias} roster />
			{:else}
				<InterfacesCatalog group={groupAlias} />
			{/if}
		{/if}
	</div>
</div>

<EnrollSheet bind:open={enrollOpen} group={groupAlias} />
