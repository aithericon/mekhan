<script lang="ts">
	import { RefreshCw } from '@lucide/svelte';

	interface Props {
		services: { handlers: string[]; categories: Record<string, string[]> } | null;
		onRefresh?: () => void;
	}

	let { services = null, onRefresh }: Props = $props();

	const categoryColors: Record<string, string> = {
		executor: 'bg-purple-500/15 text-purple-500',
		scheduler: 'bg-blue-500/15 text-blue-500',
		timer: 'bg-amber-500/15 text-amber-500',
		human: 'bg-green-500/15 text-green-500',
		custom: 'bg-gray-500/15 text-gray-500'
	};

	function colorFor(category: string): string {
		return categoryColors[category] ?? categoryColors.custom;
	}
</script>

<div class="h-full overflow-hidden flex flex-col">
	<div class="px-3 py-2 border-b border-border bg-muted shrink-0">
		<div class="flex items-center justify-between">
			<h3 class="font-semibold text-foreground text-sm">Effect Handlers</h3>
			<div class="flex items-center gap-2">
				{#if onRefresh}
					<button
						class="p-1 rounded hover:bg-accent transition-colors"
						onclick={onRefresh}
						aria-label="Refresh services"
					>
						<RefreshCw class="h-3 w-3 text-muted-foreground" />
					</button>
				{/if}
				{#if services}
					<span class="text-sm font-medium px-1.5 py-0.5 rounded-full bg-purple-500/15 text-purple-500">
						{services.handlers.length}
					</span>
				{/if}
			</div>
		</div>
	</div>
	<div class="flex-1 overflow-y-auto p-2">
		{#if !services}
			<p class="text-sm text-muted-foreground p-2">Loading...</p>
		{:else if services.handlers.length === 0}
			<div class="flex flex-col items-center justify-center py-8 text-center">
				<p class="text-sm text-muted-foreground">No effect handlers registered</p>
			</div>
		{:else}
			<div class="space-y-3">
				{#each Object.entries(services.categories) as [category, handlers] (category)}
					<div>
						<span class="text-sm font-medium px-1.5 py-0.5 rounded-full {colorFor(category)}">
							{category}
						</span>
						<div class="mt-1.5 space-y-0.5 pl-1">
							{#each handlers as handler (handler)}
								<div class="text-sm font-mono text-muted-foreground py-0.5 px-1.5 rounded hover:bg-muted transition-colors">
									{handler}
								</div>
							{/each}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
