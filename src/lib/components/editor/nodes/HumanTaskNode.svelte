<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { HumanTaskNodeData } from '$lib/types/editor';
	import User from '@lucide/svelte/icons/user';

	let { data, selected }: { data: HumanTaskNodeData; selected?: boolean } = $props();

	const stepCount = $derived(data.steps?.length ?? 0);
	const fieldCount = $derived(
		data.steps?.reduce((sum, step) => sum + step.blocks.filter((b) => b.type === 'input').length, 0) ?? 0
	);
</script>

<Handle type="target" position={Position.Left} class="!h-3 !w-3 !border-2 !border-blue-500 !bg-white" />
<div
	class="min-w-[180px] rounded-xl border-2 shadow-sm transition-shadow {selected
		? 'border-blue-500 shadow-md shadow-blue-200'
		: 'border-blue-300 shadow-sm'}"
	style="background: linear-gradient(135deg, #eff6ff, #dbeafe);"
	data-testid="node-human-task"
>
	<div class="flex items-center gap-2 border-b border-blue-200 px-3 py-2">
		<div class="flex size-6 items-center justify-center rounded-md bg-blue-500">
			<User class="size-3.5 text-white" />
		</div>
		<span class="text-sm font-medium text-blue-900">{data.label}</span>
	</div>
	<div class="px-3 py-2 text-[11px] text-blue-700">
		<div class="truncate font-medium">{data.taskTitle || 'Untitled task'}</div>
		{#if stepCount > 0 || fieldCount > 0}
			<div class="mt-0.5 text-blue-500">
				{stepCount} step{stepCount !== 1 ? 's' : ''}, {fieldCount} field{fieldCount !== 1 ? 's' : ''}
			</div>
		{/if}
	</div>
</div>
<Handle type="source" position={Position.Right} class="!h-3 !w-3 !border-2 !border-blue-500 !bg-white" />
