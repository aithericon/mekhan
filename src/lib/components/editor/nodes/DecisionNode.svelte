<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { DecisionNodeData } from '$lib/types/editor';
	import GitBranch from '@lucide/svelte/icons/git-branch';

	let { data, selected }: { data: DecisionNodeData; selected?: boolean } = $props();

	const branchCount = $derived((data.conditions?.length ?? 0) + 1); // +1 for default
</script>

<Handle type="target" position={Position.Left} class="!h-3 !w-3 !border-2 !border-amber-500 !bg-white" />
<div
	class="min-w-[160px] rotate-0 rounded-xl border-2 shadow-sm transition-shadow {selected
		? 'border-amber-500 shadow-md shadow-amber-200'
		: 'border-amber-300 shadow-sm'}"
	style="background: linear-gradient(135deg, #fffbeb, #fef3c7);"
>
	<div class="flex items-center gap-2 border-b border-amber-200 px-3 py-2">
		<div class="flex size-6 items-center justify-center rounded-md bg-amber-500">
			<GitBranch class="size-3.5 text-white" />
		</div>
		<span class="text-sm font-medium text-amber-900">{data.label}</span>
	</div>
	<div class="px-3 py-2 text-[11px] text-amber-700">
		{branchCount} branch{branchCount !== 1 ? 'es' : ''}
	</div>
</div>
<Handle
	type="source"
	position={Position.Right}
	id="default"
	class="!h-3 !w-3 !border-2 !border-amber-500 !bg-white"
/>
{#each data.conditions ?? [] as condition, i (condition.edgeId)}
	<Handle
		type="source"
		position={Position.Right}
		id={condition.edgeId}
		style="top: {30 + (i + 1) * 20}px;"
		class="!h-3 !w-3 !border-2 !border-amber-500 !bg-white"
	/>
{/each}
