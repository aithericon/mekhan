<script lang="ts">
	// Library detail layout — a shared tab strip (Nodes / Packs) over two
	// linkable child routes:
	//
	//   /library        — the node-library management list (Nodes tab)
	//   /library/packs  — installed library packs (Packs tab; nested detail at
	//                      /library/packs/[id])
	//
	// The bare /library route is exact-matched so it doesn't also light up under
	// /library/packs (a prefix of the bare path). Both child pages render only
	// their body content into the band's scroll area — the chrome (PageShell +
	// PageHeader + PageTabs) lives here so it stays identical across tabs.
	import { page } from '$app/state';
	import { PageShell, PageHeader, PageTabs, type PageTab } from '$lib/components/shell';
	import Boxes from '@lucide/svelte/icons/boxes';
	import Package from '@lucide/svelte/icons/package';

	let { children } = $props();

	const libraryTabs = $derived<PageTab[]>([
		{
			href: '/library',
			exact: true,
			label: 'Nodes',
			icon: Boxes,
			title: 'Workspace library nodes',
			testid: 'library-tab-nodes'
		},
		{
			href: '/library/packs',
			label: 'Packs',
			icon: Package,
			title: 'Installed library packs',
			testid: 'library-tab-packs'
		}
	]);

	const onPacks = $derived(page.url.pathname.startsWith('/library/packs'));
</script>

<PageShell testid="library-page">
	{#snippet band()}
		<PageHeader
			title="Library"
			subtitle={onPacks
				? 'Install, export, and manage workspace library packs'
				: 'Manage workspace library nodes — rebrand, lifecycle, and demote'}
		></PageHeader>
	{/snippet}
	{#snippet tabs()}
		<PageTabs tabs={libraryTabs} testid="library-tabs" />
	{/snippet}

	{@render children()}
</PageShell>
