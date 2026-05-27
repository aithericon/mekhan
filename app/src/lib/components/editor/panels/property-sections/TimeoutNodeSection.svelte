<script lang="ts">
	import type { TimeoutNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import GuardEditor from './GuardEditor.svelte';

	type Props = {
		data: TimeoutNodeData;
		readonly?: boolean;
		onchange: (data: TimeoutNodeData) => void;
		/** Pre-computed scope at this node — drag-insert as `<slug>.<field>`. */
		scope?: ScopeEntry[];
		/** Workflow-level resource refs surfaced as a second tab in the
		 *  embedded RefPicker (see GuardEditor). */
		resourceScope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, scope = [], resourceScope = [] }: Props = $props();
</script>

<!--
	Timeout is a body-container that races a wrapped subgraph against a
	deadline. Body work flows out the `body_in` source handle; the body's
	terminal edge targets `body_out`. Two outer outputs: default ("done")
	fires when the body wins the race (timer is cancelled); "timeout" fires
	when the timer wins (in-flight body work in cancellable children is
	drained via per-kind cancel effects).
-->
<div class="space-y-1.5">
	<div class="text-sm font-medium">Deadline (ms)</div>
	<p class="text-sm text-muted-foreground">
		Rhai expression returning the race deadline in milliseconds. The body
		must complete within this window or the <code class="font-mono">timeout</code>
		output fires and cancellable in-flight body work is drained
		(HumanTask, SubWorkflow, nested Delay).
	</p>
	<GuardEditor
		guard={data.durationMsExpr}
		{scope}
		{resourceScope}
		{readonly}
		onchange={(val) => onchange({ ...data, durationMsExpr: val })}
	/>
</div>

<div class="space-y-1.5">
	<p class="text-sm text-muted-foreground">
		<strong>v1 limitation:</strong> body cancellation reaches direct body
		children (one level deep) of cancellable kinds (HumanTask,
		SubWorkflow, Delay). AutomatedStep body children keep running until
		completion; nested Timeout/Loop body children are not auto-drained.
	</p>
</div>
