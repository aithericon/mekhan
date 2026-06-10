<script lang="ts">
	// Capacity-binding section for a HumanTask node (docs/33). A HumanTask is
	// either UNPOOLED (the historical behaviour — the task is created directly and
	// any operator can complete it from their inbox) or bound to a `human`-preset
	// `capacity` (presence · consent acceptance): the task is then OFFERED to every
	// available member enrolled in that capacity, and the first to CLAIM it binds
	// it (first-claim-wins, the rest implicitly rescinded). Placement Requirements
	// (typed constraints over a member's advertised caps) gate WHO may claim — only
	// shown once bound, since they only apply on the consent pool's `t_claim` guard.
	//
	// `capacity.alias` is the workspace path of the bound `capacity` resource; the
	// publish handler resolves it to the backing `pool-<id>` offer net. Selecting
	// "Anyone" clears the binding (and its now-meaningless requirements).
	import type { HumanTaskNodeData } from '$lib/types/editor';
	import { onMount } from 'svelte';
	import * as Select from '$lib/components/ui/select';
	import { FormField } from '$lib/components/ui/form-field';
	import { listCapacities, type CapacitySummary } from '$lib/api/capacities';
	import PlacementRequirementsSection from '../PlacementRequirementsSection.svelte';
	import Users from '@lucide/svelte/icons/users';

	type Props = {
		data: HumanTaskNodeData;
		readonly?: boolean;
		onchange: (data: HumanTaskNodeData) => void;
	};
	let { data, readonly = false, onchange }: Props = $props();

	// Sentinel for the "no capacity" option (Select values must be non-empty).
	const UNPOOLED = '__unpooled__';

	// Only consent-acceptance presence pools are valid HumanTask targets — that is
	// exactly the `human` preset (presence · consent). A runner group (presence ·
	// auto) has no consenting member, and a queue/scheduler can't take a human.
	let pools = $state<CapacitySummary[]>([]);
	onMount(() => {
		listCapacities()
			.then((cs) => {
				pools = cs.filter((c) => c.backend === 'presence' && c.axes?.acceptance === 'consent');
			})
			.catch(() => {
				pools = [];
			});
	});

	const alias = $derived(data.capacity?.alias ?? '');
	const selected = $derived(alias === '' ? UNPOOLED : alias);

	function labelFor(a: string): string {
		if (a === '') return 'Anyone — unpooled task';
		const pool = pools.find((c) => c.path === a);
		return pool ? pool.display_name || pool.path : a;
	}

	function setCapacity(v: string) {
		const next = { ...data } as Record<string, unknown>;
		if (v === UNPOOLED) {
			// Drop the binding AND its requirements — requirements are ignored
			// without a capacity, so leaving them stale would mislead.
			delete next.capacity;
			delete next.requirements;
		} else {
			next.capacity = { alias: v };
		}
		onchange(next as HumanTaskNodeData);
	}

	function setRequirements(requirements: HumanTaskNodeData['requirements'] | undefined) {
		onchange({ ...data, requirements: requirements ?? undefined });
	}
</script>

<div class="space-y-2">
	<FormField
		label="Assignment"
		for="human-capacity"
		description="Offer this task to a human-task pool, or leave it open to anyone."
	>
		<Select.Root
			type="single"
			value={selected}
			onValueChange={(v) => {
				if (v) setCapacity(v);
			}}
			disabled={readonly}
		>
			<Select.Trigger id="human-capacity" class="w-full" disabled={readonly} data-testid="select-human-capacity">
				<span class="flex items-center gap-2">
					<Users class="size-3.5 text-muted-foreground" />
					{labelFor(alias)}
				</span>
			</Select.Trigger>
			<Select.Content>
				<Select.Item value={UNPOOLED} label="Anyone — unpooled task" />
				{#each pools as pool (pool.id)}
					<Select.Item value={pool.path} label={pool.display_name || pool.path} />
				{/each}
			</Select.Content>
		</Select.Root>
	</FormField>

	{#if alias === ''}
		<p class="text-xs text-muted-foreground">
			{#if pools.length === 0}
				No human-task pools exist yet. Create a capacity with the
				<span class="font-mono">human</span> preset (presence · offer) on the Fleet page, enroll
				members, then bind it here.
			{:else}
				Bind a pool to <em>offer</em> this task to its enrolled members — the first available member
				to claim it takes it on.
			{/if}
		</p>
	{:else}
		<p class="text-xs text-muted-foreground">
			Offered to <span class="font-medium text-foreground">{labelFor(alias)}</span>; the first
			eligible member to claim it binds the task. Add Placement Requirements to restrict who may
			claim.
		</p>
		<PlacementRequirementsSection
			requirements={data.requirements}
			{readonly}
			onchange={setRequirements}
		/>
	{/if}
</div>
