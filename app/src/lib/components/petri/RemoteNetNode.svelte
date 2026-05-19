<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import { ExternalLink } from '@lucide/svelte';

	interface RemoteNetHandle {
		placeName: string;
		handleId: string;
	}

	interface Props {
		data: {
			netId: string;
			targets: RemoteNetHandle[];
			sources: RemoteNetHandle[];
			spotlightDimmed?: boolean;
			selected?: boolean;
			childNetIds?: string[];
			onNavigateToChild?: (netId: string) => void;
			onSelect?: () => void;
		};
	}

	let { data }: Props = $props();

	const hasChildren = $derived((data.childNetIds?.length ?? 0) > 0);
	const canNavigate = $derived(hasChildren && data.onNavigateToChild != null);

	function handleSelect() {
		data.onSelect?.();
	}

	function handleNavigate(e: MouseEvent) {
		e.stopPropagation();
		if (!data.onNavigateToChild) return;
		const targetId = data.childNetIds?.length
			? data.childNetIds[data.childNetIds.length - 1]
			: '';
		data.onNavigateToChild(targetId);
	}
</script>

<div
	class="remote-net-node border-2 rounded-lg w-[180px] cursor-pointer
		{data.selected ? 'selected' : ''}
		{data.spotlightDimmed ? 'spotlight-dimmed' : ''}"
	onclick={handleSelect}
	onkeydown={(e) => e.key === 'Enter' && handleSelect()}
	role="button"
	tabindex={0}
>
	<!-- Header -->
	<div class="flex items-center px-2 py-1.5 gap-1.5 min-w-0">
		<span class="text-sm font-semibold truncate mr-auto min-w-0 header-label">
			{data.netId}
		</span>
		{#if hasChildren}
			<span class="instance-badge">
				{data.childNetIds!.length}
			</span>
		{/if}
		{#if canNavigate}
			<button
				class="nav-button shrink-0 p-0.5 rounded hover:bg-teal-500/20 transition-colors"
				onclick={handleNavigate}
				title="Navigate to child net"
			>
				<ExternalLink class="w-3 h-3" style="color: hsl(172 40% 45%);" />
			</button>
		{/if}
	</div>

	<!-- Ports -->
	{#if data.targets.length > 0 || data.sources.length > 0}
		<div class="flex border-t border-border">
			<!-- Left: target handles -->
			<div class="flex flex-col justify-center items-start px-1.5 py-1">
				{#each data.targets as handle (handle.handleId)}
					<div class="port-row flex items-center gap-1" style="position: relative;">
						<Handle
							type="target"
							position={Position.Left}
							id={handle.handleId}
							class="!bg-teal-400 !w-2 !h-2"
							style="position: relative;"
						/>
						<span class="port-label text-sm font-mono truncate max-w-[60px]" title={handle.placeName}>
							{handle.placeName}
						</span>
					</div>
				{/each}
			</div>

			<div class="flex-1"></div>

			<!-- Right: source handles -->
			<div class="flex flex-col justify-center items-end px-1.5 py-1">
				{#each data.sources as handle (handle.handleId)}
					<div class="port-row flex items-center gap-1" style="position: relative;">
						<span class="port-label text-sm font-mono truncate max-w-[60px]" title={handle.placeName}>
							{handle.placeName}
						</span>
						<Handle
							type="source"
							position={Position.Right}
							id={handle.handleId}
							class="!bg-teal-400 !w-2 !h-2"
							style="position: relative;"
						/>
					</div>
				{/each}
			</div>
		</div>
	{:else}
		<Handle type="target" position={Position.Left} class="!bg-teal-400 !w-2 !h-2" />
		<Handle type="source" position={Position.Right} class="!bg-teal-400 !w-2 !h-2" />
	{/if}
</div>

<style>
	.remote-net-node {
		background: var(--card);
		border-color: hsl(172 50% 45%);
		box-shadow: 0 1px 6px rgba(0, 0, 0, 0.12);
	}

	.remote-net-node:hover {
		border-color: hsl(172 55% 38%);
		box-shadow: 0 2px 8px rgba(0, 0, 0, 0.18);
	}

	:global(.dark) .remote-net-node {
		background: hsl(172 15% 15%);
		border-color: hsl(172 40% 40%);
		box-shadow: 0 2px 10px rgba(0, 0, 0, 0.4);
	}

	:global(.dark) .remote-net-node:hover {
		border-color: hsl(172 50% 50%);
		box-shadow: 0 3px 14px rgba(0, 0, 0, 0.5);
	}

	.header-label {
		color: hsl(172 50% 30%);
	}

	:global(.dark) .header-label {
		color: hsl(172 60% 70%);
	}

	.instance-badge {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		min-width: 16px;
		height: 16px;
		padding: 0 5px;
		border-radius: 8px;
		background: hsl(172 50% 92%);
		color: hsl(172 50% 30%);
		font-size: 10px;
		font-weight: 700;
		flex-shrink: 0;
	}

	:global(.dark) .instance-badge {
		background: hsl(172 30% 22%);
		color: hsl(172 50% 65%);
	}

	.port-row {
		height: 18px;
	}

	.port-label {
		color: hsl(172 15% 50%);
	}

	:global(.dark) .port-label {
		color: hsl(172 15% 55%);
	}

	.remote-net-node.selected {
		border-color: hsl(172 60% 50%);
		box-shadow: 0 0 0 2px hsl(172 50% 45% / 0.3), 0 2px 8px rgba(0, 0, 0, 0.18);
	}

	:global(.dark) .remote-net-node.selected {
		border-color: hsl(172 60% 55%);
		box-shadow: 0 0 0 2px hsl(172 50% 50% / 0.3), 0 3px 14px rgba(0, 0, 0, 0.5);
	}

	.spotlight-dimmed {
		opacity: 0.2;
		transition: opacity 0.3s ease;
	}
</style>
