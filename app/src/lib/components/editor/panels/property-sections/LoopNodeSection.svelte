<script lang="ts">
	import type { LoopNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import GuardEditor from './GuardEditor.svelte';
	import { Input } from '$lib/components/ui/input';

	type Props = {
		data: LoopNodeData;
		readonly?: boolean;
		onchange: (data: LoopNodeData) => void;
		/** Pre-computed scope at this node — includes `<id>.iteration`. */
		scope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, scope = [] }: Props = $props();
</script>

<!--
	Loop is a `while`-with-safety-cap construct: the body runs while
	`loopCondition` is truthy, and unconditionally stops after
	`maxIterations` regardless. The condition is the primary semantic; the
	cap is a runaway guard. Surface them in that order.
-->
<div class="space-y-1.5">
	<div class="text-sm font-medium">Continue while</div>
	<p class="text-sm text-muted-foreground">
		Body runs while this guard is truthy. Default <code class="font-mono">true</code> loops
		until the safety cap below.
	</p>
	<GuardEditor
		guard={data.loopCondition}
		{scope}
		{readonly}
		onchange={(val) => onchange({ ...data, loopCondition: val })}
	/>
</div>

<div class="space-y-1.5">
	<label for="max-iterations" class="text-sm font-medium">Safety cap</label>
	<p class="text-sm text-muted-foreground">
		Hard stop after this many iterations even if the condition is still true. Protects
		against runaway loops.
	</p>
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
</div>
