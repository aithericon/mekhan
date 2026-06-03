<script lang="ts">
	// The header bar shared by the polling boards (Live board, Workers): a title,
	// a one-line summary, and a right-aligned "Updated HH:MM:SS" stamp.
	let {
		title,
		summary,
		updated = null
	}: { title: string; summary: string; updated?: Date | null } = $props();
</script>

<div class="flex items-center justify-between">
	<div class="flex items-center gap-3">
		<h3 class="text-sm font-medium text-foreground">{title}</h3>
		<span class="text-sm text-muted-foreground">{summary}</span>
	</div>
	{#if updated}
		<span class="text-sm text-muted-foreground tabular-nums">
			Updated {updated.toLocaleTimeString()}
		</span>
	{/if}
</div>
