<script lang="ts">
	// Card shell shared by every Analytics panel: title/subtitle header, an
	// optional actions slot (pills, buttons, breadcrumb), and unified
	// loading / error / empty states so the panels can't drift visually.
	import type { Snippet } from 'svelte';

	interface Props {
		title: string;
		subtitle?: string;
		loading?: boolean;
		error?: string | null;
		empty?: boolean;
		emptyMessage?: string;
		testid?: string;
		actions?: Snippet;
		children?: Snippet;
	}
	let {
		title,
		subtitle,
		loading = false,
		error = null,
		empty = false,
		emptyMessage = 'No data',
		testid,
		actions,
		children
	}: Props = $props();
</script>

<div class="rounded-xl border border-border bg-card p-4" data-testid={testid}>
	<div class="mb-3 flex flex-wrap items-start justify-between gap-2">
		<div class="min-w-0">
			<h3 class="text-sm font-semibold text-foreground">{title}</h3>
			{#if subtitle}
				<p class="mt-0.5 text-sm text-muted-foreground">{subtitle}</p>
			{/if}
		</div>
		{#if actions}
			<div class="flex shrink-0 items-center gap-2">{@render actions()}</div>
		{/if}
	</div>
	{#if loading}
		<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">Loading…</div>
	{:else if error}
		<div
			class="rounded-lg border border-rose-200 bg-rose-50 px-3 py-2 text-sm text-rose-700 dark:border-rose-900/50 dark:bg-rose-950/30 dark:text-rose-300"
		>
			{error}
		</div>
	{:else if empty}
		<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">
			{emptyMessage}
		</div>
	{:else}
		{@render children?.()}
	{/if}
</div>
