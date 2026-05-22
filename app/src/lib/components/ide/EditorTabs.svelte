<script lang="ts">
	import { onDestroy } from 'svelte';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { Awareness } from 'y-protocols/awareness';
	import type { MekhanWsProvider } from '$lib/yjs/ws-provider';
	import CollabCodeEditor from '$lib/components/editor/panels/shared/CollabCodeEditor.svelte';
	import ImageViewer from './ImageViewer.svelte';
	import X from '@lucide/svelte/icons/x';

	type TabInfo = {
		nodeId: string;
		filename: string;
		label: string;
	};

	type Props = {
		tabs: TabInfo[];
		activeTab: string | null;
		binding: YjsGraphBinding;
		awareness?: Awareness;
		provider?: MekhanWsProvider;
		onCloseTab: (key: string) => void;
		onSelectTab: (key: string) => void;
	};

	let { tabs, activeTab, binding, awareness, provider, onCloseTab, onSelectTab }: Props =
		$props();

	// The collaborative editor must only bind to a Y.Text once the server's
	// authoritative document has synced. Binding to a not-yet-synced shared
	// text makes y-codemirror mirror the local initial content back into the
	// doc, concatenating duplicate copies into the persisted file.
	let synced = $state(provider ? provider.isSynced : true);
	const handleSync = (s: boolean) => {
		synced = s;
	};
	$effect(() => {
		if (!provider) {
			synced = true;
			return;
		}
		provider.onSync(handleSync);
		return () => provider.offSync(handleSync);
	});
	onDestroy(() => provider?.offSync(handleSync));

	function tabKey(tab: TabInfo): string {
		return `${tab.nodeId}:${tab.filename}`;
	}

	const IMAGE_EXTENSIONS = ['.png', '.jpg', '.jpeg', '.gif', '.webp', '.svg'];

	function isImageFile(filename: string): boolean {
		const lower = filename.toLowerCase();
		return IMAGE_EXTENSIONS.some((ext) => lower.endsWith(ext));
	}

	function detectLanguage(filename: string): 'python' | 'json' | 'dockerfile' | 'text' {
		if (filename.endsWith('.py')) return 'python';
		if (filename.endsWith('.json')) return 'json';
		if (filename.toLowerCase() === 'dockerfile' || filename.endsWith('.dockerfile')) return 'dockerfile';
		return 'text';
	}

	const activeTabInfo = $derived(
		activeTab ? tabs.find((t) => tabKey(t) === activeTab) ?? null : null
	);

	const activeYText = $derived(
		activeTabInfo ? binding.getFileText(activeTabInfo.nodeId, activeTabInfo.filename) : null
	);

	// For image files, the Y.Text content is the S3 key — build the URL
	const activeImageSrc = $derived(
		activeTabInfo && activeYText && isImageFile(activeTabInfo.filename)
			? `/api/files/${activeYText.toString()}`
			: null
	);
</script>

<div class="flex h-full flex-col">
	{#if tabs.length === 0}
		<div class="flex flex-1 items-center justify-center text-sm text-muted-foreground">
			Select a file from the tree to start editing
		</div>
	{:else}
		<div class="flex items-center border-b border-border bg-card">
			{#each tabs as tab}
				{@const key = tabKey(tab)}
				<div
					class="group flex items-center gap-1.5 border-r border-border px-3 py-1.5 text-sm transition-colors {activeTab === key
						? 'bg-background text-foreground'
						: 'text-muted-foreground hover:bg-accent hover:text-foreground'}"
				>
					<button
						type="button"
						class="max-w-[140px] truncate text-left"
						onclick={() => onSelectTab(key)}
					>
						<span class="text-muted-foreground">[{tab.label}]</span>
						{' '}
						<span class="font-mono">{tab.filename}</span>
					</button>
					<button
						type="button"
						class="rounded p-0.5 text-muted-foreground opacity-0 transition-all group-hover:opacity-100 hover:text-foreground"
						onclick={() => onCloseTab(key)}
						title="Close tab"
					>
						<X class="size-3" />
					</button>
				</div>
			{/each}
		</div>

		<div class="flex-1 overflow-hidden">
			{#if activeTabInfo && activeImageSrc}
				{#key activeTab}
					<ImageViewer src={activeImageSrc} filename={activeTabInfo.filename} />
				{/key}
			{:else if activeYText && activeTabInfo && synced}
				{#key activeTab}
					<CollabCodeEditor
						ytext={activeYText}
						language={detectLanguage(activeTabInfo.filename)}
						{awareness}
						minHeight="100%"
						maxHeight="100%"
					/>
				{/key}
			{:else if activeYText && activeTabInfo}
				<div
					class="flex h-full items-center justify-center text-sm text-muted-foreground"
				>
					Syncing…
				</div>
			{:else}
				<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
					File not found
				</div>
			{/if}
		</div>
	{/if}
</div>
