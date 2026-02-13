<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { StartNodeData } from '$lib/types/editor';
	import Play from '@lucide/svelte/icons/play';

	let { data, selected }: { data: StartNodeData; selected?: boolean } = $props();
</script>

<div
	class="flex items-center gap-2 rounded-full border-2 px-4 py-2 shadow-sm transition-shadow {selected
		? 'border-green-500 shadow-md shadow-green-200'
		: 'border-green-400 shadow-sm'}"
	style="background: linear-gradient(135deg, #f0fdf4, #dcfce7);"
>
	<Play class="size-4 text-green-600" />
	<span class="text-sm font-medium text-green-800">{data.label}</span>
</div>
<Handle type="source" position={Position.Right} class="!h-3 !w-3 !border-2 !border-green-500 !bg-white" />
