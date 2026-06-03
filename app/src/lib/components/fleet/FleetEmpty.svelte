<script lang="ts">
	// The dashed empty-state card shared by the Fleet views. `icon` + `action` are
	// snippets so each view supplies its own lucide glyph and call-to-action.
	import type { Snippet } from 'svelte';

	let {
		message,
		hint,
		icon,
		action
	}: { message: string; hint?: string; icon?: Snippet; action?: Snippet } = $props();
</script>

<div
	class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16"
>
	{#if icon}{@render icon()}{/if}
	<p class="mt-3 text-sm text-muted-foreground">{message}</p>
	{#if hint}
		<p class="mt-1 text-sm text-muted-foreground">{hint}</p>
	{/if}
	{#if action}
		<div class="mt-4">{@render action()}</div>
	{/if}
</div>
