<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { SubWorkflowNodeData } from '$lib/types/editor';
	import Workflow from '@lucide/svelte/icons/workflow';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';
	import { NODE_WIDTH } from '$lib/editor/node-dimensions';

	let { id, data, selected }: { id: string; data: SubWorkflowNodeData; selected?: boolean } =
		$props();

	const fields = $derived(data.output?.fields ?? []);
	const hasFields = $derived(fields.length > 0);
	// Render the child's INPUT contract (its Start fields) on the node face the
	// same way a Start node renders its declared fields — so a sub-workflow
	// advertises what it *consumes*, not just the rows the author happened to
	// map. Snapshot is persisted onto `data.inputContract` by the property
	// panel's io-contract fetch and refreshed at publish (mirrors `data.output`).
	const inputFields = $derived(data.inputContract?.fields ?? []);
	const hasInputs = $derived(inputFields.length > 0);
	const outputId = $derived(data.output?.id ?? 'out');
	const pinLabel = $derived(
		data.versionPin?.mode === 'pinned' ? `v${data.versionPin.version}` : 'latest'
	);
	// `templateId` is the stable family id; the property panel carries a
	// human name once picked. Fall back to a short id / unset hint.
	const childLabel = $derived(
		data.templateId ? data.templateId.slice(0, 8) : '— pick a template —'
	);

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

<Handle
	id="in"
	type="target"
	position={Position.Left}
	class={workflowNodeHandleClass('sub-workflow')}
/>
<WorkflowNodeCard
	nodeId={id}
	kind="sub-workflow"
	icon={Workflow}
	label={data.label}
	{selected}
	width={NODE_WIDTH.sub_workflow}
	data-testid="node-sub-workflow"
	body={subWorkflowBody}
/>
{#snippet subWorkflowBody()}
	<div class="space-y-1.5" data-testid="sub-workflow-body">
		<div class="flex items-center justify-between gap-2">
			<span class="truncate font-mono text-sm text-foreground/80">{childLabel}</span>
			<span
				class="shrink-0 rounded bg-node-sub-workflow/15 px-1.5 py-0.5 text-sm font-medium uppercase text-node-sub-workflow"
			>
				{pinLabel}
			</span>
		</div>
		{#if hasInputs}
			<div class="space-y-0.5 border-t border-border/40 pt-1.5" data-testid="sub-workflow-inputs">
				<div class="flex items-center justify-between">
					<span class="text-sm uppercase tracking-wider text-muted-foreground/70">
						{data.inputContract?.label ?? 'Input'}
					</span>
					<span class="text-sm text-muted-foreground/70">
						{inputFields.length} field{inputFields.length === 1 ? '' : 's'}
					</span>
				</div>
				<ul class="space-y-0.5">
					{#each inputFields as field (field.name)}
						<li class="flex items-center justify-between gap-2">
							<span class="truncate font-mono text-sm text-foreground">
								{field.name || '—'}{field.required ? '*' : ''}
							</span>
							<span
								class="rounded bg-node-sub-workflow/15 px-1.5 py-0.5 text-sm font-medium uppercase text-node-sub-workflow"
							>
								{kindBadge[field.kind] ?? field.kind}
							</span>
						</li>
					{/each}
				</ul>
			</div>
		{/if}
		{#if hasFields}
			<div class="space-y-0.5 border-t border-border/40 pt-1.5" data-testid="sub-workflow-output">
				<div class="flex items-center justify-between">
					<span class="text-sm uppercase tracking-wider text-muted-foreground/70">
						{data.output?.label ?? 'Result'}
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
								class="rounded bg-node-sub-workflow/15 px-1.5 py-0.5 text-sm font-medium uppercase text-node-sub-workflow"
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
	class={workflowNodeHandleClass('sub-workflow')}
/>
<!-- Error output: the child failed / spawn failed. Wire to a handler or End
     to route the failure back into the graph; unconnected = dead-ends. -->
<Handle
	id="error"
	type="source"
	position={Position.Bottom}
	class="!h-3 !w-3 !border-2"
	style="background:#ef4444;border-color:#b91c1c;"
	title="On child failure"
/>
