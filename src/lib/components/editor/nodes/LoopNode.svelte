<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { LoopNodeData } from '$lib/types/editor';
	import Repeat from '@lucide/svelte/icons/repeat';

	let { data, selected }: { data: LoopNodeData; selected?: boolean } = $props();
</script>

<Handle type="target" position={Position.Left} class="!h-3 !w-3 !border-2 !border-pink-500 !bg-white" />
<div
	class="min-w-[160px] rounded-xl border-2 shadow-sm transition-shadow {selected
		? 'border-pink-500 shadow-md shadow-pink-200'
		: 'border-pink-300 shadow-sm'}"
	style="background: linear-gradient(135deg, #fdf2f8, #fce7f3);"
	data-testid="node-loop"
>
	<div class="flex items-center gap-2 border-b border-pink-200 px-3 py-2">
		<div class="flex size-6 items-center justify-center rounded-md bg-pink-500">
			<Repeat class="size-3.5 text-white" />
		</div>
		<span class="text-sm font-medium text-pink-900">{data.label}</span>
	</div>
	<div class="px-3 py-2 text-[11px] text-pink-700">
		Max {data.maxIterations ?? 3} iterations
	</div>
</div>
<Handle type="source" position={Position.Right} class="!h-3 !w-3 !border-2 !border-pink-500 !bg-white" />
