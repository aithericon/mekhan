<script lang="ts">
	import { Copy, Check } from '@lucide/svelte';
	import { cn } from '$lib/utils.js';

	let {
		text,
		// Lazy alternative to `text` — computed at click time. Use this when the
		// content is large or changes frequently (event logs, log buffers,
		// pretty-printed JSON) so we don't serialize on every render.
		getText,
		// Optional visible label rendered next to the icon (e.g. "Copy events").
		// When omitted the button is icon-only, matching the original inline use.
		label,
		title = 'Copy to clipboard',
		class: className,
		iconClass = 'w-3.5 h-3.5'
	}: {
		text?: string;
		getText?: () => string;
		label?: string;
		title?: string;
		class?: string;
		iconClass?: string;
	} = $props();

	let copied = $state(false);

	async function handleCopy() {
		const value = getText ? getText() : (text ?? '');
		await navigator.clipboard.writeText(value);
		copied = true;
		setTimeout(() => (copied = false), 1500);
	}
</script>

<button
	type="button"
	class={cn(
		'inline-flex items-center gap-1 p-1 rounded hover:bg-muted transition-colors text-muted-foreground hover:text-foreground',
		className
	)}
	onclick={handleCopy}
	title={copied ? 'Copied!' : title}
>
	{#if copied}
		<Check class={cn(iconClass, 'text-green-500')} />
	{:else}
		<Copy class={iconClass} />
	{/if}
	{#if label}
		<span class="text-sm">{copied ? 'Copied' : label}</span>
	{/if}
</button>
