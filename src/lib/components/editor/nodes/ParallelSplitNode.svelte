<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { ParallelSplitNodeData } from '$lib/types/editor';
	import GitFork from '@lucide/svelte/icons/git-fork';

	let { data, selected }: { data: ParallelSplitNodeData; selected?: boolean } = $props();
</script>

<Handle type="target" position={Position.Left} class="!h-3 !w-3 !border-2 !border-cyan-500 !bg-white" />
<div
	class="flex items-center gap-2 rounded-xl border-2 px-4 py-2.5 shadow-sm transition-shadow {selected
		? 'border-cyan-500 shadow-md shadow-cyan-200'
		: 'border-cyan-300 shadow-sm'}"
	style="background: linear-gradient(135deg, #ecfeff, #cffafe);"
	data-testid="node-parallel-split"
>
	<div class="flex size-6 items-center justify-center rounded-md bg-cyan-500">
		<GitFork class="size-3.5 text-white" />
	</div>
	<span class="text-sm font-medium text-cyan-900">{data.label}</span>
</div>
<Handle type="source" position={Position.Right} class="!h-3 !w-3 !border-2 !border-cyan-500 !bg-white" />
