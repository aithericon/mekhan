<script lang="ts">
	import type { LoopNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Info from '@lucide/svelte/icons/info';
	import GuardEditor from './GuardEditor.svelte';
	import { Input } from '$lib/components/ui/input';
	import * as Tooltip from '$lib/components/ui/tooltip';

	type Props = {
		data: LoopNodeData;
		readonly?: boolean;
		onchange: (data: LoopNodeData) => void;
		/** Pre-computed scope at this node — includes `<id>.iteration` and any
		 *  declared accumulator vars. */
		scope?: ScopeEntry[];
		/** Workflow-level resource refs surfaced as a second tab in the
		 *  embedded RefPicker (see GuardEditor). */
		resourceScope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, scope = [], resourceScope = [] }: Props = $props();

	// Accumulators are optional (older graphs / freshly-dropped nodes omit them).
	let accumulators = $derived(data.accumulators ?? []);

	function addAccumulator() {
		onchange({
			...data,
			accumulators: [...accumulators, { var: '', init: '', mergeExpr: '' }]
		});
	}

	function removeAccumulator(index: number) {
		onchange({ ...data, accumulators: accumulators.filter((_, i) => i !== index) });
	}

	function updateAccumulator(index: number, field: 'var' | 'init' | 'mergeExpr', value: string) {
		const updated = accumulators.map((a, i) => (i === index ? { ...a, [field]: value } : a));
		onchange({ ...data, accumulators: updated });
	}
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
		{resourceScope}
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

<!--
	Accumulators are fold/scan state carried across iterations — the iteration
	counter generalized. Each lives in the loop's parked envelope and is
	readable downstream as `<loop_slug>.<var>`. `init` seeds it on entry;
	`mergeExpr` refolds it each iteration and may reference the prior value
	(`<loop_slug>.<var>`) and the body's output (`<body_slug>.<field>`).
-->
<div class="space-y-2">
	<div class="flex items-center justify-between">
		<div class="flex items-center gap-1.5">
			<span class="text-sm font-medium text-muted-foreground">Accumulators</span>
			<Tooltip.Provider delayDuration={150}>
				<Tooltip.Root>
					<Tooltip.Trigger
						class="text-muted-foreground transition-colors hover:text-foreground"
						aria-label="How accumulators work"
					>
						<Info class="size-4" />
					</Tooltip.Trigger>
					<Tooltip.Content side="bottom" class="max-w-xs text-sm leading-snug">
						State folded across iterations. Read downstream as
						<code class="font-mono">{'<loop>.<var>'}</code>. The merge expression sees the
						prior value <code class="font-mono">{'<loop>.<var>'}</code> and the body's
						output <code class="font-mono">{'<body>.<field>'}</code>.
					</Tooltip.Content>
				</Tooltip.Root>
			</Tooltip.Provider>
		</div>
		{#if !readonly}
			<button
				type="button"
				class="flex items-center gap-1 rounded-md px-2 py-0.5 text-sm font-medium text-primary transition-colors hover:bg-accent"
				onclick={addAccumulator}
			>
				<Plus class="size-3" />
				Add Accumulator
			</button>
		{/if}
	</div>

	<!--
		Warm allocation is no longer authored on the Loop itself. To hold ONE
		cluster allocation across iterations (warm-start the body), drop the Loop
		inside a Lease Scope container — every step in that scope, including this
		Loop's body, then runs on the held lease.
	-->
	<p class="text-sm italic text-muted-foreground">
		To reuse one cluster allocation across iterations, wrap this Loop in a
		<span class="font-medium">Lease Scope</span> — the held allocation warms the body across runs.
	</p>

	{#each accumulators as acc, i (i)}
		<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2 text-sm">
			<div class="flex items-center gap-2">
				<Input
					type="text"
					value={acc.var}
					placeholder="variable name"
					disabled={readonly}
					oninput={(e) =>
						updateAccumulator(i, 'var', (e.currentTarget as HTMLInputElement).value)}
					class="h-7 px-2 py-1 font-mono text-sm"
				/>
				<span class="shrink-0 text-muted-foreground">=</span>
				<Input
					type="text"
					value={acc.init}
					placeholder="initial value, e.g. 0 or []"
					disabled={readonly}
					oninput={(e) =>
						updateAccumulator(i, 'init', (e.currentTarget as HTMLInputElement).value)}
					class="h-7 px-2 py-1 font-mono text-sm"
				/>
				{#if !readonly}
					<button
						type="button"
						class="shrink-0 rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
						onclick={() => removeAccumulator(i)}
						aria-label="Remove accumulator"
					>
						<Trash2 class="size-3.5" />
					</button>
				{/if}
			</div>
			<div class="space-y-1">
				<span class="text-sm text-muted-foreground">Each iteration, set to</span>
				<GuardEditor
					guard={acc.mergeExpr}
					{scope}
					{resourceScope}
					{readonly}
					onchange={(val) => updateAccumulator(i, 'mergeExpr', val)}
				/>
			</div>
		</div>
	{/each}
</div>
