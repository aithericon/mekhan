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
	//   Executor + pool   → "Pool: <alias>"
	//   Scheduled + submit → "Scheduled"
	//   Scheduled + lease  → "Lease: <scheduler>"
	// Executor without a pool shows no chip (the default, unbounded path).
	const deployChip = $derived.by(() => {
		const dm = data.deploymentModel;
		if (!dm) return null;
		if (dm.mode === 'executor') {
			if (dm.pool == null) return null;
			return { text: `Pool: ${dm.pool.alias || '—'}`, title: `Holds a unit from the "${dm.pool.alias}" token pool while running` };
		}
		// scheduled
		const op = dm.operation ?? 'submit';
		if (op === 'lease') {
			const sched = dm.scheduler ?? '';
			return { text: `Lease: ${sched || '—'}`, title: `Leases an allocation from the "${sched}" datacenter for the step's duration` };
		}
		return { text: 'Scheduled', title: 'Dispatched as a job through the scheduler-net' };
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
