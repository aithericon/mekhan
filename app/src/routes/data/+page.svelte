<script lang="ts">
	import * as Tabs from '$lib/components/ui/tabs';
	import Database from '@lucide/svelte/icons/database';
	import EntriesTab from '$lib/components/data/EntriesTab.svelte';
	import ServersTab from '$lib/components/data/ServersTab.svelte';

	// Deep-link the active tab via ?tab=servers so an entry's server link can
	// jump straight to the Servers view.
	let tab = $state(
		typeof window !== 'undefined' && new URLSearchParams(window.location.search).get('tab') === 'servers'
			? 'servers'
			: 'entries'
	);

	function onTab(v: string) {
		tab = v;
		if (typeof window !== 'undefined') {
			const url = new URL(window.location.href);
			url.searchParams.set('tab', v);
			history.replaceState(null, '', url);
		}
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
				Catalogued content and the physical copies behind it, across the file
				servers the platform tracks
			</p>
		</div>

		<Tabs.Root value={tab} onValueChange={onTab}>
			<Tabs.List class="mb-4">
				<Tabs.Trigger value="entries" data-testid="data-tab-entries">Entries</Tabs.Trigger>
				<Tabs.Trigger value="servers" data-testid="data-tab-servers">Servers</Tabs.Trigger>
			</Tabs.List>

			<Tabs.Content value="entries">
				<EntriesTab onViewServers={() => onTab('servers')} />
			</Tabs.Content>
			<Tabs.Content value="servers">
				<ServersTab />
			</Tabs.Content>
		</Tabs.Root>
	</div>
</div>
