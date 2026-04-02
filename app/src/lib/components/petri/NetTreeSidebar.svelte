<script module lang="ts">
	export interface NetMeta {
		netId: string;
		label: string;
		status: string;
		inMemory: boolean;
		parentNetId?: string;
	}

	export interface NetTreeNode {
		meta: NetMeta;
		children: NetTreeNode[];
	}
</script>

<script lang="ts">
	import CircleIcon from '@lucide/svelte/icons/circle';
	import CircleDotIcon from '@lucide/svelte/icons/circle-dot';
	import CircleCheckIcon from '@lucide/svelte/icons/circle-check';
	import BanIcon from '@lucide/svelte/icons/ban';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import Loader2Icon from '@lucide/svelte/icons/loader-2';
	import X from '@lucide/svelte/icons/x';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';

	interface Props {
		tree: NetTreeNode[];
		activeNetId: string;
		wakingNetId?: string;
		statusFilter?: 'active' | 'all';
		onSelectNet: (netId: string) => void;
		onRemoveNet: (netId: string) => void;
		onRefresh: () => void;
		onToggleFilter?: () => void;
	}

	let {
		tree,
		activeNetId,
		wakingNetId,
		statusFilter = 'active',
		onSelectNet,
		onRemoveNet,
		onRefresh,
		onToggleFilter
	}: Props = $props();

	let collapsed = $state<Set<string>>(new Set());

	function toggleCollapse(netId: string) {
		const next = new Set(collapsed);
		if (next.has(netId)) next.delete(netId);
		else next.add(netId);
		collapsed = next;
	}
</script>

<div class="flex h-full flex-col bg-muted/30">
	<div class="flex items-center justify-between border-b border-border px-3 py-2">
		<span class="text-xs font-medium text-muted-foreground uppercase tracking-wider">Nets</span>
		<div class="flex items-center gap-1">
			{#if onToggleFilter}
				<button
					class="px-1.5 py-0.5 rounded text-[10px] font-medium transition-colors
						{statusFilter === 'all'
							? 'bg-primary/10 text-primary'
							: 'text-muted-foreground hover:text-foreground hover:bg-accent'}"
					onclick={onToggleFilter}
					title={statusFilter === 'all' ? 'Show active only' : 'Show all (including completed/cancelled)'}
				>
					{statusFilter === 'all' ? 'All' : 'Active'}
				</button>
			{/if}
			<button
				class="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-accent"
				onclick={onRefresh}
				title="Refresh nets"
			>
				<RefreshCwIcon class="w-3.5 h-3.5" />
			</button>
		</div>
	</div>

	<div class="flex-1 overflow-y-auto py-1">
		{#each tree as node (node.meta.netId)}
			{@render netNode(node, 0)}
		{/each}
	</div>
</div>

{#snippet netNode(node: NetTreeNode, depth: number)}
	{@const hasChildren = node.children.length > 0}
	{@const isCollapsed = collapsed.has(node.meta.netId)}
	{@const isActive = activeNetId === node.meta.netId}

	<div
		class="group relative flex items-center gap-1 px-2 py-0.5 cursor-pointer text-xs
			{isActive ? 'bg-accent text-accent-foreground font-medium' : 'text-muted-foreground hover:bg-accent/50'}"
		style="padding-left: {8 + depth * 16}px"
		role="treeitem"
		aria-selected={isActive}
		tabindex="0"
		onclick={() => onSelectNet(node.meta.netId)}
		onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onSelectNet(node.meta.netId); } }}
	>
		{#if hasChildren}
			<button
				class="p-0 flex-shrink-0 text-muted-foreground/60 hover:text-foreground"
				onclick={(e) => { e.stopPropagation(); toggleCollapse(node.meta.netId); }}
				title={isCollapsed ? 'Expand' : 'Collapse'}
			>
				<ChevronRightIcon class="size-3 transition-transform {isCollapsed ? '' : 'rotate-90'}" />
			</button>
		{:else}
			<span class="size-3 flex-shrink-0"></span>
		{/if}

		{#if wakingNetId === node.meta.netId}
			<Loader2Icon class="size-3 flex-shrink-0 text-primary animate-spin" />
		{:else if node.meta.status === 'completed'}
			<CircleCheckIcon class="size-3 flex-shrink-0 text-green-400/70" />
		{:else if node.meta.status === 'cancelled'}
			<BanIcon class="size-3 flex-shrink-0 text-muted-foreground/50" />
		{:else if node.meta.status === 'running' && node.meta.inMemory}
			<CircleDotIcon class="size-3 flex-shrink-0 text-green-500" />
		{:else if node.meta.status === 'running'}
			<CircleIcon class="size-3 flex-shrink-0 text-gray-400" />
		{:else}
			<CircleIcon class="size-3 flex-shrink-0 text-yellow-500" />
		{/if}

		<span class="truncate">{node.meta.label}</span>

		<button
			class="absolute right-1 p-0.5 rounded opacity-0 group-hover:opacity-100 text-muted-foreground hover:text-red-400"
			onclick={(e) => { e.stopPropagation(); onRemoveNet(node.meta.netId); }}
			title="Delete net"
		>
			<X class="w-3 h-3" />
		</button>
	</div>

	{#if hasChildren && !isCollapsed}
		{#each node.children as child (child.meta.netId)}
			{@render netNode(child, depth + 1)}
		{/each}
	{/if}
{/snippet}
