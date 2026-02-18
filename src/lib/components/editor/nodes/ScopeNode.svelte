<script lang="ts">
	import { NodeResizer } from '@xyflow/svelte';
	import type { ScopeNodeData } from '$lib/types/editor';

	let { data, selected }: { data: ScopeNodeData; selected?: boolean } = $props();
</script>

<NodeResizer
	isVisible={selected}
	minWidth={200}
	minHeight={120}
/>
<div
	class="h-full w-full rounded-xl border-2 border-dashed {selected
		? 'border-slate-400 bg-slate-50/60'
		: 'border-slate-300/70 bg-slate-50/30'}"
	data-testid="node-scope"
>
	<span class="absolute left-3 top-2 text-[10px] font-medium uppercase tracking-wider text-slate-400">
		{data.label}
	</span>
</div>
