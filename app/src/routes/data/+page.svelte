<script lang="ts">
	import * as Tabs from '$lib/components/ui/tabs';
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

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-6xl px-6 py-8 animate-rise">
		<div class="mb-6">
			<div class="flex items-center gap-2">
				<Database class="size-6 text-muted-foreground" />
				<h1 class="text-2xl font-semibold tracking-tight text-foreground">Data</h1>
			</div>
			<p class="mt-1 text-sm text-muted-foreground">
				Catalogued content, the physical copies behind it, and the file servers
				the platform tracks — one browser.
			</p>
		</div>

		<Tabs.Root value={tab} onValueChange={onTab}>
			<Tabs.List class="mb-4">
				<Tabs.Trigger value="entries" data-testid="data-tab-entries">Entries</Tabs.Trigger>
				<Tabs.Trigger value="copies" data-testid="data-tab-copies">Copies</Tabs.Trigger>
				<Tabs.Trigger value="servers" data-testid="data-tab-servers">Servers</Tabs.Trigger>
			</Tabs.List>

			<Tabs.Content value="entries">
				<EntriesTab onViewServer={viewServer} />
			</Tabs.Content>
			<Tabs.Content value="copies">
				<CopiesTab onViewServer={viewServer} />
			</Tabs.Content>
			<Tabs.Content value="servers">
				<ServersTab focusKey={focusServer} />
			</Tabs.Content>
		</Tabs.Root>
	</div>
</div>
