<script lang="ts">
	import { ExternalLink } from '@lucide/svelte';
	import { Separator } from '$lib/components/ui/separator';
	import { Card } from '$lib/components/ui/card';
	import NodeKindBadge from '../NodeKindBadge.svelte';

	interface RemoteNetSelection {
		id: string;
		label: string;
		targets: string[];
		sources: string[];
		childNetIds: string[];
	}

	interface Props {
		rn: RemoteNetSelection;
		onNavigateToChild?: (netId: string) => void;
	}

	let { rn, onNavigateToChild }: Props = $props();
</script>

<div class="space-y-4">
	<Card tone="muted">
		<h3 class="text-lg font-medium text-foreground">{rn.label}</h3>
		<p class="text-sm text-muted-foreground font-mono">{rn.id}</p>
		<div class="flex items-center gap-2 mt-2">
			<NodeKindBadge kind="remote_net" />
			{#if rn.childNetIds.length > 0}
				<span class="text-sm text-muted-foreground">
					{rn.childNetIds.length} {rn.childNetIds.length === 1 ? 'instance' : 'instances'}
				</span>
			{/if}
		</div>
	</Card>

	<Separator />

	<!-- Bridge Ports -->
	{#if rn.targets.length > 0}
		<Card tone="muted">
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-2">
				Outbound Ports ({rn.targets.length})
			</h4>
			<div class="space-y-1">
				{#each rn.targets as port (port)}
					<div class="px-2 py-1 rounded border border-border flex items-center gap-2">
						<span class="w-2 h-2 rounded-full bg-destructive shrink-0"></span>
						<span class="text-sm font-mono text-foreground truncate">{port}</span>
					</div>
				{/each}
			</div>
		</Card>
	{/if}

	{#if rn.sources.length > 0}
		<Card tone="muted">
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-2">
				Inbound Ports ({rn.sources.length})
			</h4>
			<div class="space-y-1">
				{#each rn.sources as port (port)}
					<div class="px-2 py-1 rounded border border-border flex items-center gap-2">
						<span class="w-2 h-2 rounded-full bg-success shrink-0"></span>
						<span class="text-sm font-mono text-foreground truncate">{port}</span>
					</div>
				{/each}
			</div>
		</Card>
	{/if}

	{#if rn.childNetIds.length > 0}
		<Separator />

		<!-- Child Net Instances -->
		<Card tone="muted">
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-2">
				Child Instances ({rn.childNetIds.length})
			</h4>
			<div class="space-y-1 max-h-48 overflow-y-auto">
				{#each rn.childNetIds as childId (childId)}
					<button
						class="w-full text-left px-2 py-1.5 rounded border border-border hover:border-success/50 hover:bg-success/10 transition-colors flex items-center gap-2"
						onclick={() => onNavigateToChild?.(childId)}
					>
						<span class="text-sm font-mono text-foreground truncate">{childId.slice(0, 12)}...</span>
						<ExternalLink class="w-3 h-3 ml-auto shrink-0 text-success" />
					</button>
				{/each}
			</div>
		</Card>
	{:else}
		<Separator />
		<div class="text-sm text-muted-foreground italic text-center py-2">
			No child instances spawned yet
		</div>
	{/if}
</div>
