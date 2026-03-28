<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';

	interface PortInfo {
		id: string;
		label: string;
	}

	interface MetaGroupData {
		label: string;
		placeCount: number;
		transitionCount: number;
		tokenCount: number;
		inputPorts: PortInfo[];
		outputPorts: PortInfo[];
		selected?: boolean;
		spotlightDimmed?: boolean;
		onSelect?: () => void;
		/** True if this group represents a spawn (dynamic child net). */
		isSpawn?: boolean;
		/** Number of active child net instances. */
		spawnInstanceCount?: number;
		/** Child net IDs for navigation. */
		spawnChildNetIds?: string[];
		/** Navigate to a child net tab. */
		onNavigateToChild?: (netId: string) => void;
	}

	interface Props {
		data: MetaGroupData;
	}

	let { data }: Props = $props();

	function handleClick() {
		if (data.isSpawn && data.spawnChildNetIds?.length === 1 && data.onNavigateToChild) {
			data.onNavigateToChild(data.spawnChildNetIds[0]);
		} else {
			data.onSelect?.();
		}
	}
</script>

<div
	class="meta-group-node rounded-lg border-2 cursor-pointer
		{data.isSpawn ? 'spawn-border' : ''}
		{data.selected ? 'ring-2 ring-primary ring-offset-1 ring-offset-background' : ''}
		{data.spotlightDimmed ? 'spotlight-dimmed' : ''}"
	onclick={handleClick}
	onkeydown={(e) => e.key === 'Enter' && handleClick()}
	role="button"
	tabindex="0"
	style="width: 220px;"
>
	<!-- Header -->
	<div class="meta-header px-3 py-1.5" class:spawn-header={data.isSpawn}>
		<div class="text-xs font-bold truncate meta-title flex items-center gap-1">
			{#if data.isSpawn}<span class="spawn-badge" title="Spawned subnet">&#x21BB;</span>{/if}
			{data.label}
		</div>
		<div class="meta-summary text-[10px] font-mono">
			{#if data.isSpawn && data.spawnInstanceCount != null}
				{data.spawnInstanceCount} instance{data.spawnInstanceCount !== 1 ? 's' : ''}
				{#if data.tokenCount > 0}<span class="meta-token-count"> · {data.tokenCount} tok</span>{/if}
			{:else}
				{data.placeCount}P · {data.transitionCount}T{#if data.tokenCount > 0}<span class="meta-token-count"> · {data.tokenCount} tok</span>{/if}
			{/if}
		</div>
	</div>

	<!-- Ports -->
	{#if data.inputPorts.length > 0 || data.outputPorts.length > 0}
		<div class="meta-ports flex border-t">
			<!-- Input ports (left) -->
			<div class="flex flex-col justify-center items-start px-1.5 py-1">
				{#each data.inputPorts as port (port.id)}
					<div class="port-row flex items-center gap-1" style="position: relative;">
						<Handle
							type="target"
							position={Position.Left}
							id={port.id}
							class="!bg-blue-400 !w-2 !h-2"
							style="position: relative;"
						/>
						<span class="port-label text-[9px] font-mono truncate max-w-[70px]" title={port.label}>
							{port.label}
						</span>
					</div>
				{/each}
			</div>

			<div class="flex-1"></div>

			<!-- Output ports (right) -->
			<div class="flex flex-col justify-center items-end px-1.5 py-1">
				{#each data.outputPorts as port (port.id)}
					<div class="port-row flex items-center gap-1" style="position: relative;">
						<span class="port-label text-[9px] font-mono truncate max-w-[70px]" title={port.label}>
							{port.label}
						</span>
						<Handle
							type="source"
							position={Position.Right}
							id={port.id}
							class="!bg-green-400 !w-2 !h-2"
							style="position: relative;"
						/>
					</div>
				{/each}
			</div>
		</div>
	{:else}
		<Handle type="target" position={Position.Left} class="!bg-gray-400 !w-2 !h-2" />
		<Handle type="source" position={Position.Right} class="!bg-gray-400 !w-2 !h-2" />
	{/if}
</div>

<style>
	.meta-group-node {
		background: var(--card);
		border-color: hsl(211 40% 58%);
		box-shadow: 0 1px 6px rgba(0, 0, 0, 0.12);
	}

	.meta-group-node.spawn-border {
		border-color: hsl(280 40% 58%);
		border-style: dashed;
	}

	.meta-group-node:hover {
		border-color: hsl(211 50% 50%);
		box-shadow: 0 2px 8px rgba(0, 0, 0, 0.18);
		transform: scale(1.01);
		transition: all 0.1s ease;
	}

	.meta-group-node.spawn-border:hover {
		border-color: hsl(280 50% 50%);
	}

	:global(.dark) .meta-group-node {
		background: hsl(215 20% 16%);
		border-color: hsl(211 40% 45%);
		box-shadow: 0 2px 10px rgba(0, 0, 0, 0.4);
	}

	:global(.dark) .meta-group-node.spawn-border {
		border-color: hsl(280 35% 50%);
	}

	:global(.dark) .meta-group-node:hover {
		border-color: hsl(211 50% 55%);
		box-shadow: 0 3px 14px rgba(0, 0, 0, 0.5);
	}

	:global(.dark) .meta-group-node.spawn-border:hover {
		border-color: hsl(280 45% 60%);
	}

	.meta-header {
		background: hsl(211 40% 95%);
		border-radius: 6px 6px 0 0;
	}

	.meta-header.spawn-header {
		background: hsl(280 35% 94%);
	}

	:global(.dark) .meta-header {
		background: hsl(211 25% 22%);
	}

	:global(.dark) .meta-header.spawn-header {
		background: hsl(280 25% 20%);
	}

	.spawn-badge {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 14px;
		height: 14px;
		border-radius: 3px;
		background: hsl(280 50% 60%);
		color: white;
		font-size: 10px;
		font-weight: bold;
		flex-shrink: 0;
	}

	:global(.dark) .spawn-badge {
		background: hsl(280 40% 50%);
	}

	.meta-title {
		color: hsl(211 50% 35%);
	}

	:global(.dark) .meta-title {
		color: hsl(211 60% 75%);
	}

	.meta-summary {
		color: hsl(211 20% 55%);
	}

	:global(.dark) .meta-summary {
		color: hsl(211 20% 55%);
	}

	.meta-token-count {
		color: hsl(142 50% 40%);
		font-weight: 600;
	}

	:global(.dark) .meta-token-count {
		color: hsl(142 50% 60%);
	}

	.meta-ports {
		border-color: hsl(211 20% 88%);
	}

	:global(.dark) .meta-ports {
		border-color: hsl(211 15% 28%);
	}

	.port-label {
		color: hsl(215 15% 50%);
	}

	:global(.dark) .port-label {
		color: hsl(215 15% 55%);
	}

	.port-row {
		height: 16px;
	}

	.spotlight-dimmed {
		opacity: 0.2;
		transition: opacity 0.3s ease;
	}
</style>
