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
		? 'border-node-scope bg-node-scope/10'
		: 'border-node-scope/50 bg-node-scope/5'}"
	data-testid="node-scope"
>
	<span class="absolute left-3 top-2 text-sm font-medium uppercase tracking-wider text-muted-foreground">
		{data.label}
	</span>
</div>
