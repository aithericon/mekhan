<script lang="ts">
	import { getContext } from 'svelte';
	import { NodeResizer } from '@xyflow/svelte';
	import type { ScopeNodeData } from '$lib/types/editor';
	import {
		RESIZE_REPORT_CONTEXT_KEY,
		type ResizeReport
	} from './resize-context';

	let { id, data, selected }: { id: string; data: ScopeNodeData; selected?: boolean } = $props();

	// Absent on readonly canvases / standalone previews — fall back to no-op
	// so the resizer never crashes if rendered outside WorkflowCanvas.
	const reportResize = getContext<ResizeReport | undefined>(RESIZE_REPORT_CONTEXT_KEY);
</script>

<NodeResizer
	isVisible={selected && !!reportResize}
	minWidth={200}
	minHeight={120}
	onResizeEnd={(_e, params) => reportResize?.(id, params)}
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
