<script lang="ts">
	import type { LoopNodeData } from '$lib/types/editor';
	import CodeEditor from '../shared/CodeEditor.svelte';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';

	type Props = {
		data: LoopNodeData;
		readonly?: boolean;
		onchange: (data: LoopNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();
</script>

<FormField label="Max Iterations" for="max-iterations">
	<Input
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
	/>
</FormField>

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
