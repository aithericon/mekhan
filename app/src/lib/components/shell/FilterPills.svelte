<script lang="ts" module>
	export type FilterPill = {
		/** Stable value compared against `active`. */
		value: string;
		label: string;
		/**
		 * When set the pill renders as an <a> (URL-driven filters — the filter
		 * lives in searchParams and survives reload/share). When absent the
		 * pill is a <button> and `onSelect(value)` fires (local-state filters).
		 */
		href?: string;
		testid?: string;
	};
</script>

<script lang="ts">
	// Compact pill nav for FILTER switching over the same view (status / mode
	// scopes like Instances' Live | Drafts | Test runs). Not for navigation
	// (use PageTabs) and not for content panels (use ui/tabs).
	import { cn } from '$lib/utils.js';

	let {
		options,
		active,
		onSelect,
		testid,
		class: className
	}: {
		options: FilterPill[];
		/** The currently-active option's `value`. */
		active: string;
		/** Required for button-mode options (no `href`). */
		onSelect?: (value: string) => void;
		/** data-testid on the <nav>. */
		testid?: string;
		class?: string;
	} = $props();

	const pillClass = (isActive: boolean) =>
		cn(
			'rounded px-2 py-1 transition-colors',
			isActive
				? 'bg-primary text-primary-foreground'
				: 'text-muted-foreground hover:bg-accent hover:text-foreground'
		);
</script>

<nav
	class={cn(
		'flex w-fit items-center gap-1 rounded-md border border-border bg-card p-0.5 text-xs',
		className
	)}
	data-testid={testid}
>
	{#each options as opt (opt.value)}
		{@const isActive = opt.value === active}
		{#if opt.href}
			<a
				href={opt.href}
				class={pillClass(isActive)}
				data-testid={opt.testid}
				aria-current={isActive ? 'page' : undefined}
			>
				{opt.label}
			</a>
		{:else}
			<button
				type="button"
				class={pillClass(isActive)}
				data-testid={opt.testid}
				aria-pressed={isActive}
				onclick={() => onSelect?.(opt.value)}
			>
				{opt.label}
			</button>
		{/if}
	{/each}
</nav>
