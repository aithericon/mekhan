<script lang="ts">
	import type { DelayNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import GuardEditor from './GuardEditor.svelte';

	type Props = {
		data: DelayNodeData;
		readonly?: boolean;
		onchange: (data: DelayNodeData) => void;
		/** Pre-computed scope at this node — drag-insert as `<slug>.<field>`. */
		scope?: ScopeEntry[];
		/** Workflow-level resource refs surfaced as a second tab in the
		 *  embedded RefPicker (see GuardEditor). */
		resourceScope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, scope = [], resourceScope = [] }: Props = $props();
</script>

<!--
	Delay is fire-and-forget: the workflow pauses for `durationMsExpr`
	milliseconds, then continues on the single output. The expression is
	Rhai-evaluated against the inbound control token at firing time, so
	authors can drive the delay off `input.<field>` (Start-resident) or
	`<slug>.<field>` upstream parked refs.
-->
<div class="space-y-1.5">
	<div class="text-sm font-medium">Wait for (ms)</div>
	<p class="text-sm text-muted-foreground">
		Rhai expression returning the delay in milliseconds. Plain numbers like
		<code class="font-mono">5000</code> work; refs like
		<code class="font-mono">order.sla_ms</code> resolve against upstream
		parked data via standard read-arc synthesis.
	</p>
	<GuardEditor
		guard={data.durationMsExpr}
		{scope}
		{resourceScope}
		{readonly}
		onchange={(val) => onchange({ ...data, durationMsExpr: val })}
	/>
</div>
