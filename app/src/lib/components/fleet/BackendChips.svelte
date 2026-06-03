<script lang="ts">
	// A wrapped list of executor-backend badges — the set-membership dimension a
	// runner/worker advertises (what it can run). Shared by the runner stations,
	// the runner detail drawer, the group-section "covers" line, and the worker
	// cards, which all rendered their own near-identical badge loops at differing
	// sizes. One renderer → one consistent size (text-sm, never smaller).
	import { Badge } from '$lib/components/ui/badge';

	let {
		backends,
		variant = 'outline',
		empty
	}: {
		backends: string[];
		/** Badge style — 'outline' for advisory chips, 'secondary' for emphasis. */
		variant?: 'outline' | 'secondary';
		/** Text to show when there are no backends. Omit to render nothing. */
		empty?: string;
	} = $props();
</script>

{#if backends.length > 0}
	<div class="flex flex-wrap gap-1.5">
		{#each backends as be (be)}
			<Badge {variant} class="text-sm font-normal">{be}</Badge>
		{/each}
	</div>
{:else if empty}
	<span class="text-sm text-muted-foreground">{empty}</span>
{/if}
