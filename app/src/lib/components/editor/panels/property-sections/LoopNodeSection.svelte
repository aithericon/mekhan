<script lang="ts">
	import type { LoopNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import GuardEditor from './GuardEditor.svelte';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';

	type Props = {
		data: LoopNodeData;
		readonly?: boolean;
		onchange: (data: LoopNodeData) => void;
		/** Pre-computed scope at this node — includes `<id>.iteration`. */
		scope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, scope = [] }: Props = $props();
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

<div>
	<GuardEditor
		guard={data.loopCondition}
		{scope}
		{readonly}
		onchange={(val) => onchange({ ...data, loopCondition: val })}
	/>
</div>
