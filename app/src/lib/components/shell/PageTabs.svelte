<script lang="ts" module>
	import type { PageIcon } from './PageHeader.svelte';

	export type PageTab = {
		/** Real subroute the tab navigates to (linkable / bookmarkable). */
		href: string;
		label: string;
		/**
		 * Pathname prefix that marks the tab active. Defaults to `href` without
		 * its query string (matched via `pathname.startsWith`).
		 */
		match?: string;
		icon?: PageIcon;
		/** Hover tooltip. */
		title?: string;
		testid?: string;
	};
</script>

<script lang="ts">
	// URL-driven link tabs for ROUTE-LEVEL navigation: each tab is a real <a>
	// to a subroute (navigation unmounts the previous view; deep-linkable).
	// Styled to be pixel-compatible with the shadcn Tabs trigger, so state
	// tabs (ui/tabs) and link tabs look identical. For same-URL content
	// switching use ui/tabs; for list filters use FilterPills.
	import { page } from '$app/state';
	import { cn } from '$lib/utils.js';

	let {
		tabs,
		testid,
		class: className
	}: {
		tabs: PageTab[];
		/** data-testid on the <nav>. */
		testid?: string;
		class?: string;
	} = $props();

	const pathname = $derived(page.url.pathname);
	const isActive = (tab: PageTab) => pathname.startsWith(tab.match ?? tab.href.split('?')[0]);
</script>

<nav
	class={cn(
		'inline-flex h-9 w-fit items-center justify-center rounded-lg bg-muted p-[3px] text-muted-foreground',
		className
	)}
	data-testid={testid}
>
	{#each tabs as tab (tab.href)}
		{@const active = isActive(tab)}
		{@const Icon = tab.icon}
		<a
			href={tab.href}
			title={tab.title}
			data-testid={tab.testid}
			data-state={active ? 'active' : 'inactive'}
			aria-current={active ? 'page' : undefined}
			class="inline-flex items-center justify-center gap-1.5 rounded-md border border-transparent px-2.5 py-1 text-sm font-medium whitespace-nowrap text-foreground transition-[color,box-shadow] data-[state=active]:bg-background data-[state=active]:shadow-sm dark:text-muted-foreground dark:data-[state=active]:border-input dark:data-[state=active]:bg-input/30 dark:data-[state=active]:text-foreground [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4"
		>
			{#if Icon}
				<Icon class="size-3.5" />
			{/if}
			{tab.label}
		</a>
	{/each}
</nav>
