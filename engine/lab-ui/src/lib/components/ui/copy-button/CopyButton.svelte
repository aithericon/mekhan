<script lang="ts">
	import { Copy, Check } from '@lucide/svelte';
	import { cn } from '$lib/utils.js';

	let { text, class: className }: { text: string; class?: string } = $props();

	let copied = $state(false);

	async function handleCopy() {
		await navigator.clipboard.writeText(text);
		copied = true;
		setTimeout(() => (copied = false), 1500);
	}
</script>

<button
	class={cn(
		'p-1 rounded hover:bg-muted transition-colors text-muted-foreground hover:text-foreground',
		className
	)}
	onclick={handleCopy}
	title="Copy to clipboard"
>
	{#if copied}
		<Check class="w-3.5 h-3.5 text-green-500" />
	{:else}
		<Copy class="w-3.5 h-3.5" />
	{/if}
</button>
