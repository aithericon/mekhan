<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import Cpu from '@lucide/svelte/icons/cpu';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';
	import { NODE_WIDTH } from '$lib/editor/node-dimensions';

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

	// Streaming Channels (docs/25). Each declared channel becomes one in-body
	// row, and its handle is rendered INSIDE that row (xyflow measures handle
	// positions against the node, so a nested handle anchors to its row) — the
	// port lines up with its badge by construction, no percentage math. Rows
	// stack vertically so a node with many channels grows tall, not wide. IN
	// channels hug the left edge (target handle), OUT channels hug the right
	// (source). The handle `id` MUST equal `channel.name` — that's the wiring
	// contract edges resolve against.
	type Channel = NonNullable<AutomatedStepNodeData['channels']>[number];
	const channels = $derived<Channel[]>(data.channels ?? []);

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
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('automated')} />
<WorkflowNodeCard
	nodeId={id}
	kind="automated"
	icon={Cpu}
	label={data.label}
	{selected}
	width={NODE_WIDTH.automated_step}
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
			<!-- Channels stack vertically; each row carries its own handle at the
			     matching edge (target=left for IN, source=right for OUT) so the
			     port aligns with its badge. `-mx-3` makes the rows full-bleed to
			     the card edge so the nested handles sit on the node border; the
			     badge keeps `px-3` to stay aligned with the body content above. -->
			<div class="-mx-3 space-y-1 border-t border-border/40 pt-1.5" data-testid="automated-step-channels">
				<span class="block px-3 text-sm uppercase tracking-wider text-muted-foreground/70">Channels</span>
				<ul class="space-y-1">
					{#each channels as channel (channel.name)}
						<li
							class="relative flex px-3 {channel.direction === 'out'
								? 'justify-end'
								: 'justify-start'}"
						>
							<Handle
								id={channel.name}
								type={channel.direction === 'out' ? 'source' : 'target'}
								position={channel.direction === 'out' ? Position.Right : Position.Left}
								class="!h-3 !w-3 !border-2"
								style={channelStyle(channel.plane)}
								title={`Channel ${channel.name} — ${channelTitle(channel)}`}
							/>
							<span
								class="inline-flex max-w-full items-center gap-1 rounded px-1.5 py-0.5 text-sm font-medium {channel.plane ===
								'data'
									? 'bg-amber-500/15 text-amber-600 dark:text-amber-400'
									: 'bg-purple-500/15 text-purple-600 dark:text-purple-400'}"
								title={channelTitle(channel)}
							>
								<span class="text-muted-foreground/70">{channel.direction === 'out' ? '→' : '←'}</span>
								<span class="truncate font-mono">{channel.name}</span>
								<span class="text-muted-foreground/60">{elementLabel(channel.element)}</span>
							</span>
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
	class="!h-3 !w-3 !border-2"
	style="background:#ef4444;border-color:#b91c1c;"
	title="On error (retries exhausted)"
/>
<!-- Streaming Channel handles (docs/25) are rendered inside each channel row in
     the card body (see the `channels` block in the snippet above): OUT channels
     get a source handle on the right edge, IN channels a target handle on the
     left, each vertically aligned with its badge. The handle `id` == channel.name
     is the wiring contract edges resolve against. -->

