<script lang="ts">
	import * as Tabs from '$lib/components/ui/tabs';
	import { PageShell, PageHeader } from '$lib/components/shell';
	import Database from '@lucide/svelte/icons/database';
	import EntriesTab from '$lib/components/data/EntriesTab.svelte';
	import CopiesTab from '$lib/components/data/CopiesTab.svelte';
	import ServersTab from '$lib/components/data/ServersTab.svelte';

	const TABS = ['entries', 'copies', 'servers'];

	// Deep-link the active tab via ?tab= so a server link can jump straight to
	// the Servers view (and bookmarks survive). /catalogue + /inventory redirect
	// here — this one page is the whole Data surface.
	function initialTab(): string {
		if (typeof window === 'undefined') return 'entries';
		const t = new URLSearchParams(window.location.search).get('tab');
		return t && TABS.includes(t) ? t : 'entries';
	}

	let tab = $state(initialTab());
	// Server key to highlight when jumping to the Servers tab from a copy link.
	let focusServer = $state<string | null>(null);

	function onTab(v: string) {
		tab = v;
		if (typeof window !== 'undefined') {
			const url = new URL(window.location.href);
			url.searchParams.set('tab', v);
			history.replaceState(null, '', url);
		}
	}

	function viewServer(key?: string) {
		focusServer = key ?? null;
		onTab('servers');
	}
</script>

<!-- Tabs.Root wraps the WHOLE shell so the trigger row can live in the band's
     `tabs` snippet while the panels stay in the scrolling body (bits-ui
     context spans both). -->
<Tabs.Root value={tab} onValueChange={onTab} class="h-full gap-0">
	<PageShell width="wide" testid="data-page">
		{#snippet band()}
			<PageHeader
				title="Data"
				icon={Database}
				subtitle="Catalogued content, the physical copies behind it, and the file servers the platform tracks — one browser."
			/>
		{/snippet}
		{#snippet tabs()}
			<Tabs.List variant="underline">
				<Tabs.Trigger variant="underline" value="entries" data-testid="data-tab-entries">
					Entries
				</Tabs.Trigger>
				<Tabs.Trigger variant="underline" value="copies" data-testid="data-tab-copies">
					Copies
				</Tabs.Trigger>
				<Tabs.Trigger variant="underline" value="servers" data-testid="data-tab-servers">
					Servers
				</Tabs.Trigger>
			</Tabs.List>
		{/snippet}

		<Tabs.Content value="entries">
			<EntriesTab onViewServer={viewServer} />
		</Tabs.Content>
		<Tabs.Content value="copies">
			<CopiesTab onViewServer={viewServer} />
		</Tabs.Content>
		<Tabs.Content value="servers">
			<ServersTab focusKey={focusServer} />
		</Tabs.Content>
	</PageShell>
</Tabs.Root>
