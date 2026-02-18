<script lang="ts">
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { Awareness } from 'y-protocols/awareness';
	import CollabCodeEditor from '$lib/components/editor/panels/shared/CollabCodeEditor.svelte';
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
		onCloseTab: (key: string) => void;
		onSelectTab: (key: string) => void;
	};

	let { tabs, activeTab, binding, awareness, onCloseTab, onSelectTab }: Props = $props();

	function tabKey(tab: TabInfo): string {
		return `${tab.nodeId}:${tab.filename}`;
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
					class="group flex items-center gap-1.5 border-r border-border px-3 py-1.5 text-xs transition-colors {activeTab === key
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
			{#if activeYText && activeTabInfo}
				{#key activeTab}
					<CollabCodeEditor
						ytext={activeYText}
						language={detectLanguage(activeTabInfo.filename)}
						{awareness}
						minHeight="100%"
						maxHeight="100%"
					/>
				{/key}
			{:else}
				<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
					File not found
				</div>
			{/if}
		</div>
	{/if}
</div>
