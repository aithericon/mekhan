<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { DecisionNodeData } from '$lib/types/editor';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: DecisionNodeData; selected?: boolean } = $props();

	// Ordered, labelled branch list: each condition in author order, then the
	// default ("Otherwise") branch last. `handleId` is the xyflow source-handle
	// id and MUST stay equal to the edge's sourceHandle (`condition.edgeId` /
	// `"default"`) so existing graphs keep their wiring — this redesign is
	// purely visual (one labelled row per branch, port aligned to its row).
	const branches = $derived([
		...(data.conditions ?? []).map((c, i) => ({
			handleId: c.edgeId,
			label: c.label?.trim() || `Branch ${i + 1}`
		})),
		...(data.defaultBranch ? [{ handleId: 'default', label: 'Otherwise' }] : [])
	]);
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('decision')} />
<WorkflowNodeCard
	nodeId={id}
	kind="decision"
	icon={GitBranch}
	label={data.label}
	{selected}
	class="min-w-[200px]"
	data-testid="node-decision"
	body={branchBody}
/>
{#snippet branchBody()}
	{#if branches.length === 0}
		<span class="text-muted-foreground">No branches</span>
	{:else}
		<!-- -mx-3 cancels the card body's px-3 so a row's right edge is the
		     node's inner border; the handle is then centred on that border at
		     the row's own vertical middle (xyflow derives the connection point
		     from the handle element's real position, so no header math). -->
		<div class="-mx-3 flex flex-col gap-1">
			{#each branches as branch, i (branch.handleId)}
				<div
					class="relative flex h-6 items-center gap-2 bg-node-decision/10 px-3"
					title={branch.label}
				>
					<span
						class="flex size-4 shrink-0 items-center justify-center rounded-sm bg-node-decision/70 text-sm font-semibold text-white"
					>
						{i + 1}
					</span>
					<span class="flex-1 truncate text-sm font-medium text-foreground">
						{branch.label}
					</span>
					<Handle
						type="source"
						position={Position.Right}
						id={branch.handleId}
						class={workflowNodeHandleClass('decision') + ' !absolute'}
						style="top:50%;right:0;transform:translate(50%,-50%);"
					/>
				</div>
			{/each}
		</div>
	{/if}
{/snippet}
