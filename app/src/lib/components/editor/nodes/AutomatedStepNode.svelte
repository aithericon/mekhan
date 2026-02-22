<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import Cpu from '@lucide/svelte/icons/cpu';

	let { data, selected }: { data: AutomatedStepNodeData; selected?: boolean } = $props();
</script>

<Handle type="target" position={Position.Left} class="!h-3 !w-3 !border-2 !border-violet-500 !bg-white" />
<div
	class="min-w-[180px] rounded-xl border-2 shadow-sm transition-shadow {selected
		? 'border-violet-500 shadow-md shadow-violet-200'
		: 'border-violet-300 shadow-sm'}"
	style="background: linear-gradient(135deg, #f5f3ff, #ede9fe);"
	data-testid="node-automated-step"
>
	<div class="flex items-center gap-2 border-b border-violet-200 px-3 py-2">
		<div class="flex size-6 items-center justify-center rounded-md bg-violet-500">
			<Cpu class="size-3.5 text-white" />
		</div>
		<span class="text-sm font-medium text-violet-900">{data.label}</span>
	</div>
	<div class="px-3 py-2 text-[11px] text-violet-700">
		<div class="truncate capitalize">{data.executionSpec?.backendType ?? 'python'}</div>
	</div>
</div>
<Handle type="source" position={Position.Right} class="!h-3 !w-3 !border-2 !border-violet-500 !bg-white" />
