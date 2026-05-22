<script lang="ts">
	import { Separator } from '$lib/components/ui/separator';
	import { Card } from '$lib/components/ui/card';
	import { Badge } from '$lib/components/ui/badge';
	import NodeKindBadge from '../NodeKindBadge.svelte';

	// The store currently projects only `{ group }`; the richer fields below
	// (places / transitions / childGroups / allTokens) are referenced exactly
	// as the original Inspector did — preserved verbatim, not "fixed".
	interface Props {
		groupDetails: any;
		onSelectPlace?: (id: string) => void;
		onSelectTransition?: (id: string) => void;
		onSelectToken?: (placeId: string, tokenId: string) => void;
		onSelectGroup?: (id: string) => void;
	}

	let {
		groupDetails,
		onSelectPlace,
		onSelectTransition,
		onSelectToken,
		onSelectGroup
	}: Props = $props();
</script>

<div class="space-y-4">
	<Card tone="muted">
		<h3 class="text-lg font-medium text-foreground">{groupDetails.group.name}</h3>
		<p class="text-sm text-muted-foreground font-mono">{groupDetails.group.id}</p>
		<div class="flex items-center gap-2 mt-2">
			<NodeKindBadge kind="group" />
			<span class="text-sm text-muted-foreground">
				{groupDetails.places.length} places · {groupDetails.transitions.length} transitions
			</span>
		</div>
	</Card>

	{#if groupDetails.childGroups.length > 0}
		<Separator />
		<Card tone="muted">
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-2">
				Sub-groups ({groupDetails.childGroups.length})
			</h4>
			<div class="space-y-1">
				{#each groupDetails.childGroups as child (child.id)}
					<button
						class="w-full text-left px-2 py-1 rounded border border-border hover:border-primary/50 hover:bg-primary/10 transition-colors"
						onclick={() => onSelectGroup?.(child.id)}
					>
						<span class="text-sm font-medium text-foreground">{child.name}</span>
					</button>
				{/each}
			</div>
		</Card>
	{/if}

	<Separator />

	<!-- Places in group -->
	<Card tone="muted">
		<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-2">
			Places ({groupDetails.places.length})
		</h4>
		<div class="space-y-1 max-h-40 overflow-y-auto">
			{#each groupDetails.places as place (place.id)}
				{@const count = groupDetails.allTokens.filter((t: any) => t.placeId === place.id).length}
				<button
					class="w-full text-left px-2 py-1 rounded border border-border hover:border-primary/50 hover:bg-primary/10 transition-colors flex items-center gap-2"
					onclick={() => onSelectPlace?.(place.id)}
				>
					<span class="text-sm font-medium text-foreground truncate">{place.name}</span>
					{#if count > 0}
						<span class="ml-auto text-sm font-mono px-1.5 py-0.5 rounded-full bg-primary/15 text-primary shrink-0">
							{count}
						</span>
					{/if}
				</button>
			{/each}
		</div>
	</Card>

	<Separator />

	<!-- Tokens across all places -->
	<Card tone="muted">
		<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-2">
			Tokens ({groupDetails.allTokens.length})
		</h4>
		{#if groupDetails.allTokens.length === 0}
			<p class="text-sm text-muted-foreground italic">No tokens in this group</p>
		{:else}
			<div class="space-y-2 max-h-64 overflow-y-auto">
				{#each groupDetails.allTokens as { placeId, placeName, token } (token.id)}
					<button
						class="w-full text-left p-2 rounded border border-l-2 border-l-primary/50 border-border hover:border-primary/50 hover:bg-primary/10 transition-colors"
						onclick={() => onSelectToken?.(placeId, token.id)}
					>
						<div class="flex items-start gap-2">
							<span class="text-sm px-1.5 py-0.5 rounded bg-muted text-muted-foreground font-medium shrink-0">
								{token.color.type}
							</span>
							<div class="flex-1 min-w-0">
								{#if token.color.type === 'Unit'}
									<span class="text-sm text-muted-foreground italic">empty</span>
								{:else if token.color.type === 'Data'}
									<pre class="text-sm text-foreground/80 truncate">{JSON.stringify(token.color.value)}</pre>
								{:else}
									<span class="text-sm font-mono text-primary">{token.color.value}</span>
								{/if}
							</div>
						</div>
						<div class="text-sm font-mono text-muted-foreground mt-1">
							<span class="text-primary/60">{placeName}</span> · {token.id.slice(0, 8)}...
						</div>
					</button>
				{/each}
			</div>
		{/if}
	</Card>

	<Separator />

	<!-- Transitions in group -->
	<Card tone="muted">
		<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-2">
			Transitions ({groupDetails.transitions.length})
		</h4>
		<div class="space-y-1 max-h-40 overflow-y-auto">
			{#each groupDetails.transitions as transition (transition.id)}
				<button
					class="w-full text-left px-2 py-1 rounded border border-border hover:border-primary/50 hover:bg-primary/10 transition-colors flex items-center gap-2"
					onclick={() => onSelectTransition?.(transition.id)}
				>
					<span class="text-sm font-medium text-foreground truncate">{transition.name}</span>
					{#if transition.effect_handler_id}
						<Badge variant="secondary" size="xs" class="ml-auto font-mono shrink-0">FX</Badge>
					{/if}
				</button>
			{/each}
		</div>
	</Card>
</div>
