<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { StreamSourceNodeData } from '$lib/types/editor';
	import RadioTower from '@lucide/svelte/icons/radio-tower';
	import WorkflowNodeCard from './WorkflowNodeCard.svelte';
	import { NODE_WIDTH } from '$lib/editor/node-dimensions';

	let { id, data, selected }: { id: string; data: StreamSourceNodeData; selected?: boolean } =
		$props();

	// A StreamSource is a push-ingress endpoint: external producers POST bytes
	// at a stable per-instance URL and the node emits them into the net as its
	// declared OUT channels. It has NO control-flow ports at all (the backend
	// NodeDecl declares input_ports=[] and no default out handle) — its ONLY
	// handles are the per-channel source handles rendered inside each channel
	// row, exactly like AutomatedStepNode's channel rows: the handle `id` MUST
	// equal `channel.name` (the wiring contract edges resolve against), and
	// nesting the handle in its row makes xyflow anchor the port next to its
	// badge by construction.
	type Channel = NonNullable<StreamSourceNodeData['channels']>[number];
	const channels = $derived<Channel[]>(data.channels ?? []);

	// element.type → short label; binary carries the content_type (e.g. audio/wav).
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
	icon={RadioTower}
	label={data.label}
	{selected}
	width={NODE_WIDTH.stream_source}
	data-testid="node-stream-source"
	body={sourceBody}
/>
{#snippet sourceBody()}
	<div class="space-y-1.5" data-testid="stream-source-body">
		<span class="block truncate text-foreground/80">Ingress endpoint</span>
		{#if channels.length === 0}
			<span class="block text-sm italic text-muted-foreground/70">No channels declared</span>
		{:else}
			<!-- Channel rows stack vertically; each carries its own source handle on
			     the right edge so the port aligns with its badge. `-mx-3` makes the
			     rows full-bleed so the nested handles sit on the node border. -->
			<div class="-mx-3 space-y-1 border-t border-border/40 pt-1.5" data-testid="stream-source-channels">
				<ul class="space-y-1">
					{#each channels as channel (channel.name)}
						<li class="relative flex px-3">
							<Handle
								id={channel.name}
								type="source"
								position={Position.Right}
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
								<span class="shrink-0 text-muted-foreground/70">→</span>
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
