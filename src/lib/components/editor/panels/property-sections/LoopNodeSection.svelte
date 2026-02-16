<script lang="ts">
	import type { LoopNodeData } from '$lib/types/editor';
	import CodeEditor from '../shared/CodeEditor.svelte';

	type Props = {
		data: LoopNodeData;
		readonly?: boolean;
		onchange: (data: LoopNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();
</script>

<div class="space-y-1.5">
	<label for="max-iterations" class="text-xs font-medium text-muted-foreground"
		>Max Iterations</label
	>
	<input
		id="max-iterations"
		type="number"
		min={1}
		value={data.maxIterations}
		disabled={readonly}
		oninput={(e) =>
			onchange({
				...data,
				maxIterations: parseInt((e.currentTarget as HTMLInputElement).value) || 1
			})}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Loop Condition (Rhai)</span>
	<CodeEditor
		value={data.loopCondition}
		language="rhai"
		{readonly}
		minHeight="40px"
		maxHeight="120px"
		onchange={(val) => onchange({ ...data, loopCondition: val })}
	/>
</div>
