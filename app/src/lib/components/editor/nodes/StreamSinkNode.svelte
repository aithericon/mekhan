<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { StreamSinkNodeData } from '$lib/types/editor';
	import SatelliteDish from '@lucide/svelte/icons/satellite-dish';
	import WorkflowNodeCard from './WorkflowNodeCard.svelte';
	import { NODE_WIDTH } from '$lib/editor/node-dimensions';

	let { id, data, selected }: { id: string; data: StreamSinkNodeData; selected?: boolean } =
		$props();

	// A StreamSink is a pull-egress endpoint: it consumes its single declared IN
	// channel and re-exposes the bytes at a stable per-instance URL. It has NO
	// control-flow ports (the backend NodeDecl declares output_ports=[] and no
	// default in handle) — its ONLY handle is the per-channel target handle on
	// the left, rendered inside the channel row exactly like AutomatedStepNode's
	// channel rows (handle `id` == `channel.name` is the wiring contract).
	type Channel = NonNullable<StreamSinkNodeData['channels']>[number];
	const channels = $derived<Channel[]>(data.channels ?? []);

	function elementLabel(el: Channel['element']): string {
		if (el.type === 'binary') return el.content_type || 'binary';
		return el.type; // 'json' | 'any'
	}

	// Plane palette mirrors AutomatedStepNode: data rides amber, control purple.
	function channelStyle(plane: Channel['plane']): string {
		return plane === 'data'
			? 'background:#f59e0b;border-color:#b45309;'
			: 'background:#a855f7;border-color:#7e22ce;';
	}
	function channelTitle(c: Channel): string {
		return `${c.direction} · ${c.plane} · ${elementLabel(c.element)}`;
	}
</script>

<WorkflowNodeCard
	nodeId={id}
	kind="stream"
	icon={SatelliteDish}
	label={data.label}
	{selected}
	width={NODE_WIDTH.stream_sink}
	data-testid="node-stream-sink"
	body={sinkBody}
/>
{#snippet sinkBody()}
	<div class="space-y-1.5" data-testid="stream-sink-body">
		<span class="block truncate text-foreground/80">Egress endpoint</span>
		{#if channels.length === 0}
			<span class="block text-sm italic text-muted-foreground/70">No channel declared</span>
		{:else}
			<div class="-mx-3 space-y-1 border-t border-border/40 pt-1.5" data-testid="stream-sink-channels">
				<ul class="space-y-1">
					{#each channels as channel (channel.name)}
						<li class="relative flex px-3">
							<Handle
								id={channel.name}
								type="target"
								position={Position.Left}
								class="!h-3 !w-3 !border-2"
								style={channelStyle(channel.plane)}
								title={`Channel ${channel.name} — ${channelTitle(channel)}`}
							/>
							<span
								class="flex w-full items-center gap-1 rounded px-1.5 py-0.5 text-sm font-medium {channel.plane ===
								'data'
									? 'bg-amber-500/15 text-amber-600 dark:text-amber-400'
									: 'bg-purple-500/15 text-purple-600 dark:text-purple-400'}"
								title={channelTitle(channel)}
							>
								<span class="shrink-0 text-muted-foreground/70">←</span>
								<span class="min-w-0 flex-1 truncate font-mono" title={channel.name}>{channel.name}</span>
								<span
									class="min-w-0 max-w-[45%] shrink-0 truncate text-muted-foreground/60"
									title={elementLabel(channel.element)}>{elementLabel(channel.element)}</span>
							</span>
						</li>
					{/each}
				</ul>
			</div>
		{/if}
	</div>
{/snippet}
