<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { EndNodeData } from '$lib/types/editor';
	import Square from '@lucide/svelte/icons/square';

	let { data, selected }: { data: EndNodeData; selected?: boolean } = $props();
</script>

<Handle type="target" position={Position.Left} class="!h-3 !w-3 !border-2 !border-red-500 !bg-white" />
<div
	class="flex items-center gap-2 rounded-full border-2 px-4 py-2 shadow-sm transition-shadow {selected
		? 'border-red-500 shadow-md shadow-red-200'
		: 'border-red-400 shadow-sm'}"
	style="background: linear-gradient(135deg, #fef2f2, #fecaca);"
	data-testid="node-end"
>
	<Square class="size-4 text-red-600" />
	<span class="text-sm font-medium text-red-800">{data.label}</span>
</div>
