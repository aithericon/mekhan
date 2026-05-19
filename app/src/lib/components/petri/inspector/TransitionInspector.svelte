<script lang="ts">
	import { Pencil } from '@lucide/svelte';
	import { Separator } from '$lib/components/ui/separator';
	import { Card } from '$lib/components/ui/card';
	import { Button } from '$lib/components/ui/button';
	import NodeKindBadge from '../NodeKindBadge.svelte';
	import type { TransitionDetails } from '$lib/stores/inspector-selectors';

	interface Props {
		transitionDetails: TransitionDetails;
		getPlaceName?: (id: string) => string;
		onSelectPlace?: (id: string) => void;
		onOpenScript?: () => void;
	}

	let { transitionDetails, getPlaceName, onSelectPlace, onOpenScript }: Props = $props();
</script>

<div class="space-y-4">
	<Card tone="muted">
		<h3 class="text-lg font-medium text-foreground">{transitionDetails.transition.name}</h3>
		<p class="text-sm text-muted-foreground font-mono">{transitionDetails.transition.id}</p>
		<div class="flex items-center gap-2 mt-2">
			{#if transitionDetails.transition.effect_handler_id}
				<NodeKindBadge kind="effect" />
				<span class="text-sm font-mono text-muted-foreground">
					{transitionDetails.transition.effect_handler_id}
				</span>
			{:else}
				<NodeKindBadge kind="rhai" />
			{/if}
		</div>
	</Card>

	<Separator />

	<!-- Effect Handler -->
	{#if transitionDetails.transition.effect_handler_id}
		<Card tone="muted">
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-2">Effect Handler</h4>
			<div class="px-3 py-2 rounded text-sm bg-secondary border border-border text-secondary-foreground font-mono">
				{transitionDetails.transition.effect_handler_id}
			</div>
			<p class="text-sm text-muted-foreground mt-2">
				Runs a registered side-effect handler instead of a Rhai script.
			</p>
		</Card>

		<Separator />
	{/if}

	<!-- Guard -->
	{#if true}
		{@const guardScript = transitionDetails.transition.guard}
		<Card tone="muted">
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-2">Guard Condition</h4>
			<div
				class="px-3 py-2 rounded text-sm font-mono {guardScript
					? 'bg-warning/10 border border-warning/30 text-warning-foreground'
					: 'bg-muted text-muted-foreground'}"
			>
				{guardScript ?? 'None (always enabled)'}
			</div>
		</Card>
	{/if}

	<Separator />

	<!-- Input Places -->
	<Card tone="muted">
		<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-2">
			Input Places ({transitionDetails.inputArcs.length})
		</h4>
		{#if transitionDetails.inputArcs.length === 0}
			<p class="text-sm text-muted-foreground italic">None</p>
		{:else}
			<ul class="space-y-1">
				{#each transitionDetails.inputArcs as arc (arc.place_id)}
					<li>
						<Button variant="link" size="inline" class="text-sm" onclick={() => onSelectPlace?.(arc.place_id)}>
							{getPlaceName?.(arc.place_id) ?? arc.place_name ?? arc.place_id}
						</Button>
						{#if arc.weight && arc.weight > 1}
							<span class="text-sm text-muted-foreground">(weight: {arc.weight})</span>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	</Card>

	<!-- Output Places -->
	<Card tone="muted">
		<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-2">
			Output Places ({transitionDetails.outputArcs.length})
		</h4>
		{#if transitionDetails.outputArcs.length === 0}
			<p class="text-sm text-muted-foreground italic">None</p>
		{:else}
			<ul class="space-y-1">
				{#each transitionDetails.outputArcs as arc (arc.place_id)}
					<li>
						<Button variant="link" size="inline" class="text-sm" onclick={() => onSelectPlace?.(arc.place_id)}>
							{getPlaceName?.(arc.place_id) ?? arc.place_name ?? arc.place_id}
						</Button>
						{#if arc.weight && arc.weight > 1}
							<span class="text-sm text-muted-foreground">(weight: {arc.weight})</span>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	</Card>

	<!-- Open Script/Effect Sheet -->
	{#if onOpenScript}
		<Button onclick={onOpenScript} class="w-full">
			<Pencil class="w-4 h-4" />
			View / Edit Logic
		</Button>
	{/if}
</div>
