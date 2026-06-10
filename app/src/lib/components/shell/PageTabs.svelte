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
	// GitHub-style underline tabs — quiet inline labels with a 2px active
	// underline, designed to sit flush on the PageShell band's border-b (the
	// shell's `tabs` snippet wrapper pulls the row down with -mb-px so the
	// underline overlaps the band border). Pixel-compatible with ui/tabs
	// variant="underline", so link tabs and page-level state tabs look
	// identical. For same-URL content switching use ui/tabs; for list
	// filters use FilterPills.
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

<nav class={cn('flex w-fit items-end gap-1', className)} data-testid={testid}>
	{#each tabs as tab (tab.href)}
		{@const active = isActive(tab)}
		{@const Icon = tab.icon}
		<a
			href={tab.href}
			title={tab.title}
			data-testid={tab.testid}
			data-state={active ? 'active' : 'inactive'}
			aria-current={active ? 'page' : undefined}
			class={cn(
				'inline-flex items-center justify-center gap-1.5 border-b-2 px-3 py-2 text-sm font-medium whitespace-nowrap transition-colors [&_svg]:pointer-events-none [&_svg]:shrink-0',
				active
					? 'border-primary text-foreground'
					: 'border-transparent text-muted-foreground hover:border-border hover:text-foreground'
			)}
		>
			{#if Icon}
				<Icon class="size-4" />
			{/if}
			{tab.label}
		</a>
	{/each}
</nav>
