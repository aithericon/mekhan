<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import Cpu from '@lucide/svelte/icons/cpu';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: AutomatedStepNodeData; selected?: boolean } = $props();

	// Phase 2 typed-ports: render declared `output` port fields inline so the
	// port editor's effect is visible on the canvas. Falls back to the legacy
	// compact card (just backend name) when output has no declared fields.
	const fields = $derived(data.output?.fields ?? []);
	const hasFields = $derived(fields.length > 0);
	const outputId = $derived(data.output?.id ?? 'out');
	// Deployment chip — surfaces the resource binding at a glance:
	//   Executor + capacity → "Capacity: <alias>"
	//   Scheduled + submit  → "Scheduled"
	//   Scheduled + lease   → "Lease: <scheduler>"
	// Executor without a capacity binding shows no chip (the default, unbounded path).
	const deployChip = $derived.by(() => {
		const dm = data.deploymentModel;
		if (!dm) return null;
		if (dm.mode === 'executor') {
			if (dm.capacity == null) return null;
			return { text: `Capacity: ${dm.capacity.alias || '—'}`, title: `Holds a unit from the "${dm.capacity.alias}" capacity while running` };
		}
		// scheduled — always lease pattern now
		const sched = dm.scheduler ?? '';
		if (sched) {
			return { text: `Lease: ${sched}`, title: `Leases an allocation from the "${sched}" datacenter for the step's duration` };
		}
		return { text: 'Scheduled', title: 'Dispatched as a job through an external cluster (Nomad/Slurm)' };
	});

	const kindBadge: Record<string, string> = {
		text: 'Txt',
		textarea: 'Txt',
		number: 'Num',
		bool: 'Bool',
		select: 'Sel',
		file: 'File',
		signature: 'Sig',
		timestamp: 'Time',
		json: 'JSON'
	};

	// Streaming Channels (docs/25). Each declared channel becomes a handle on the
	// node edge + an in-body badge. We split in/out so they stack down opposite
	// edges (target=left, source=right) without colliding with the fixed
	// `in`/`out`/`error` handles. The handle `id` MUST equal `channel.name` —
	// that's the wiring contract edges resolve against.
	type Channel = NonNullable<AutomatedStepNodeData['channels']>[number];
	const channels = $derived<Channel[]>(data.channels ?? []);
	const outChannels = $derived(channels.filter((c) => c.direction === 'out'));
	const inChannels = $derived(channels.filter((c) => c.direction === 'in'));

	// element.type → short label; binary carries the content_type (e.g. audio/wav).
	function elementLabel(el: Channel['element']): string {
		if (el.type === 'binary') return el.content_type || 'binary';
		return el.type; // 'json' | 'any'
	}

	// Data plane rides amber, control plane rides purple (matches the legacy
	// control-out purple `#a855f7`). Tooltip reads "direction · plane · element".
	function channelStyle(plane: ChannelPlane): string {
		return plane === 'data'
			? 'background:#f59e0b;border-color:#b45309;'
			: 'background:#a855f7;border-color:#7e22ce;';
	}
	function channelTitle(c: Channel): string {
		return `${c.direction} · ${c.plane} · ${elementLabel(c.element)}`;
	}
	type ChannelPlane = Channel['plane'];

	// Per-edge stacking: distribute N handles evenly down the side, starting
	// below the fixed handle, so multiple channels never overlap. xyflow
	// positions handles against the node bounding box, so `top` is a percentage
	// of node height.
	function handleTop(index: number, total: number): string {
		// Spread within the lower band (60%..92%), clearing the fixed in/out
		// handle which xyflow centers at ~50% — so the first channel doesn't
		// overlap (and become hard to grab on) the primary port.
		const top = 60;
		const span = 32;
		const step = total > 1 ? span / (total - 1) : 0;
		return `${top + index * step}%`;
	}
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('automated')} />
<WorkflowNodeCard
	nodeId={id}
	kind="automated"
	icon={Cpu}
	label={data.label}
	{selected}
	class="min-w-[200px]"
	data-testid="node-automated-step"
	body={automatedBody}
/>
{#snippet automatedBody()}
	<div class="space-y-1.5" data-testid="automated-step-body">
		<div class="flex items-center justify-between gap-2">
			<span class="truncate capitalize text-foreground/80">
				{data.executionSpec?.backendType ?? 'python'}
			</span>
			{#if deployChip}
				<span
					class="inline-flex shrink-0 items-center gap-1 rounded bg-node-automated/15 px-1.5 py-0.5 text-sm font-medium text-node-automated"
					title={deployChip.title}
					data-testid="badge-deployment"
				>
					<Cpu class="size-3" />
					{deployChip.text}
				</span>
			{/if}
		</div>
		{#if hasFields}
			<div class="space-y-0.5 border-t border-border/40 pt-1.5">
				<div class="flex items-center justify-between">
					<span class="text-sm uppercase tracking-wider text-muted-foreground/70">
						{data.output?.label ?? 'Output'}
					</span>
					<span class="text-sm text-muted-foreground/70">
						{fields.length} field{fields.length === 1 ? '' : 's'}
					</span>
				</div>
				<ul class="space-y-0.5">
					{#each fields as field (field.name)}
						<li class="flex items-center justify-between gap-2">
							<span class="truncate font-mono text-sm text-foreground">
								{field.name || '—'}{field.required ? '*' : ''}
							</span>
							<span
								class="rounded bg-node-automated/15 px-1.5 py-0.5 text-sm font-medium uppercase text-node-automated"
							>
								{kindBadge[field.kind] ?? field.kind}
							</span>
						</li>
					{/each}
				</ul>
			</div>
		{/if}
		{#if channels.length > 0}
			<div class="space-y-0.5 border-t border-border/40 pt-1.5" data-testid="automated-step-channels">
				<span class="text-sm uppercase tracking-wider text-muted-foreground/70">Channels</span>
				<ul class="flex flex-wrap gap-1">
					{#each channels as channel (channel.name)}
						<li
							class="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-sm font-medium {channel.plane ===
							'data'
								? 'bg-amber-500/15 text-amber-600 dark:text-amber-400'
								: 'bg-purple-500/15 text-purple-600 dark:text-purple-400'}"
							title={channelTitle(channel)}
						>
							<span
								class="inline-block size-2 shrink-0 rounded-full"
								style={channelStyle(channel.plane)}
							></span>
							<span class="text-muted-foreground/70">{channel.direction === 'out' ? '→' : '←'}</span>
							<span class="truncate font-mono">{channel.name}</span>
							<span class="text-muted-foreground/60">{elementLabel(channel.element)}</span>
						</li>
					{/each}
				</ul>
			</div>
		{/if}
	</div>
{/snippet}
<Handle
	id={outputId}
	type="source"
	position={Position.Right}
	class={workflowNodeHandleClass('automated')}
/>
<!-- Error output: wire this to a handler (notification, human task, End)
     to route failures (retries exhausted) back into the graph. Unconnected
     = the step dead-ends on failure. -->
<Handle
	id="error"
	type="source"
	position={Position.Bottom}
	style="background:#ef4444;border-color:#b91c1c;"
	title="On error (retries exhausted)"
/>
<!-- Streaming Channel handles (docs/25): every declared channel gets a per-name
     port. OUT channels emit at runtime (`emit`/`scatter`) — source handles on
     the right edge; downstream edges wire by `sourceHandle == channel.name`. IN
     channels consume — target handles on the left; edges wire by
     `targetHandle == channel.name`. Both stack down their edge below the fixed
     `out`/`in` handle. Color encodes plane (purple=control, amber=data). -->
{#each outChannels as channel, i (channel.name)}
	<Handle
		id={channel.name}
		type="source"
		position={Position.Right}
		style="top:{handleTop(i, outChannels.length)};{channelStyle(channel.plane)}"
		title={`Channel ${channel.name} — ${channelTitle(channel)}`}
	/>
{/each}
{#each inChannels as channel, i (channel.name)}
	<Handle
		id={channel.name}
		type="target"
		position={Position.Left}
		style="top:{handleTop(i, inChannels.length)};{channelStyle(channel.plane)}"
		title={`Channel ${channel.name} — ${channelTitle(channel)}`}
	/>
{/each}
