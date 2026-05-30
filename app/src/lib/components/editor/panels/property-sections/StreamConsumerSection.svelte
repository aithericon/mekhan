<script lang="ts">
	// StreamConsumer: drains the stream side-channel from an upstream
	// AutomatedStep with `streamOutput: true` and folds the chunks into a single
	// output token via the selected reduce strategy.
	import type { StreamConsumerNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import { FormField } from '$lib/components/ui/form-field';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import * as Select from '$lib/components/ui/select';

	type StreamReduce = components['schemas']['StreamReduce'];
	type StreamDispatch = components['schemas']['StreamDispatch'];

	type Props = {
		data: StreamConsumerNodeData;
		readonly?: boolean;
		onchange: (data: StreamConsumerNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();

	const reduce = $derived((data.reduce ?? { kind: 'array' }) as StreamReduce);
	const reduceKind = $derived(reduce.kind);

	const dispatch = $derived((data.dispatch ?? { mode: 'rhai' }) as StreamDispatch);
	const dispatchMode = $derived(dispatch.mode);

	const kindLabels: Record<StreamReduce['kind'], string> = {
		array: 'Array (ordered list of chunks)',
		concat: 'Concat (join into a string)',
		sum: 'Sum (numeric total)',
		custom: 'Custom (Rhai expression)'
	};

	const dispatchLabels: Record<StreamDispatch['mode'], string> = {
		rhai: 'Rhai (fold chunks directly, no body)',
		sequentialBody: 'Sequential body (Python per chunk, in order)',
		parallelBody: 'Parallel body (Python per chunk, map-style)',
		liveReduce: 'Live reduce (one long-lived Python loop)'
	};

	function setKind(kind: StreamReduce['kind']) {
		let next: StreamReduce;
		switch (kind) {
			case 'array': next = { kind: 'array' }; break;
			case 'concat': next = { kind: 'concat' }; break;
			case 'sum':    next = { kind: 'sum' };   break;
			case 'custom': next = { kind: 'custom', expr: '' }; break;
		}
		onchange({ ...data, reduce: next });
	}

	function setDispatch(mode: StreamDispatch['mode']) {
		onchange({ ...data, dispatch: { mode } as StreamDispatch });
	}
</script>

<!--
	Result variable: the field name each chunk's value is projected onto.
	Documentary for v1 (the compiler reads it to name the parked output).
-->
<FormField label="Result variable" for="sc-result-var">
	<Input
		id="sc-result-var"
		type="text"
		value={data.resultVar ?? 'item'}
		placeholder="item"
		disabled={readonly}
		oninput={(e) => onchange({ ...data, resultVar: (e.currentTarget as HTMLInputElement).value })}
		class="font-mono"
	/>
	<p class="mt-1 text-sm text-muted-foreground">
		Name of the output field that holds the reduced result. Borrowable downstream
		as <code class="font-mono">&lt;slug&gt;.{data.resultVar ?? 'item'}</code>.
	</p>
</FormField>

<!--
	Dispatch mode: how each drained chunk is handled BEFORE the reduce.
	Rhai folds chunks directly; the body modes run a Python child per chunk
	(wire it via the body_in/body_out handles); liveReduce runs one long-lived
	Python loop that owns the reduce itself.
-->
<FormField label="Dispatch" for="sc-dispatch-mode">
	<Select.Root
		type="single"
		value={dispatchMode}
		onValueChange={(v) => {
			if (v) setDispatch(v as StreamDispatch['mode']);
		}}
		disabled={readonly}
	>
		<Select.Trigger id="sc-dispatch-mode" class="w-full" disabled={readonly}>
			{dispatchLabels[dispatchMode]}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="rhai"           label="Rhai (fold chunks directly, no body)" />
			<Select.Item value="sequentialBody" label="Sequential body (Python per chunk, in order)" />
			<Select.Item value="parallelBody"   label="Parallel body (Python per chunk, map-style)" />
			<Select.Item value="liveReduce"     label="Live reduce (one long-lived Python loop)" />
		</Select.Content>
	</Select.Root>
	{#if dispatchMode === 'sequentialBody' || dispatchMode === 'parallelBody'}
		<p class="mt-1 text-sm text-muted-foreground">
			Each chunk runs a Python body child. Wire the body via the
			<code class="font-mono">body_in</code> / <code class="font-mono">body_out</code>
			handles. The {dispatchMode === 'sequentialBody' ? 'in-order' : 'concurrent'} results
			are then folded by the reduce below.
		</p>
	{:else if dispatchMode === 'liveReduce'}
		<p class="mt-1 text-sm text-muted-foreground">
			One long-lived Python loop is fed chunks and produces the result itself —
			the reduce is managed by the Python loop, not the picker below.
		</p>
	{/if}
</FormField>

{#if dispatchMode !== 'liveReduce'}
<!--
	Reduce strategy: how the drained chunks (or per-chunk body results) are folded
	into the single output token. Hidden for liveReduce — the Python loop reduces.
-->
<FormField label="Reduce strategy" for="sc-reduce-kind">
	<Select.Root
		type="single"
		value={reduceKind}
		onValueChange={(v) => {
			if (v) setKind(v as StreamReduce['kind']);
		}}
		disabled={readonly}
	>
		<Select.Trigger id="sc-reduce-kind" class="w-full" disabled={readonly}>
			{kindLabels[reduceKind]}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="array"  label="Array (ordered list of chunks)" />
			<Select.Item value="concat" label="Concat (join into a string)" />
			<Select.Item value="sum"    label="Sum (numeric total)" />
			<Select.Item value="custom" label="Custom (Rhai expression)" />
		</Select.Content>
	</Select.Root>
</FormField>

{#if reduceKind === 'concat'}
	<FormField label="Separator (optional)" for="sc-sep">
		<Input
			id="sc-sep"
			type="text"
			value={(reduce as { kind: 'concat'; sep?: string | null }).sep ?? ''}
			placeholder='e.g.  or \n'
			disabled={readonly}
			oninput={(e) => {
				const sep = (e.currentTarget as HTMLInputElement).value;
				onchange({
					...data,
					reduce: { kind: 'concat', sep: sep === '' ? undefined : sep }
				});
			}}
			class="font-mono"
		/>
		<p class="mt-1 text-sm text-muted-foreground">
			String inserted between each chunk. Leave empty for no separator.
		</p>
	</FormField>
{/if}

{#if reduceKind === 'custom'}
	<FormField label="Rhai expression" for="sc-expr">
		<Textarea
			id="sc-expr"
			value={(reduce as { kind: 'custom'; expr: string }).expr ?? ''}
			placeholder="chunks.reduce(|acc, c| acc + c.value, 0)"
			disabled={readonly}
			rows={3}
			oninput={(e) => {
				const expr = (e.currentTarget as HTMLTextAreaElement).value;
				onchange({ ...data, reduce: { kind: 'custom', expr } });
			}}
			class="font-mono"
		/>
		<p class="mt-1 text-sm text-muted-foreground">
			Rhai expression over <code class="font-mono">chunks</code> (an array of chunk objects).
			Must return the reduced value.
		</p>
	</FormField>
{/if}
{/if}

<p class="text-sm italic text-muted-foreground">
	The consumer drains all chunks from the stream handle before emitting via
	<code class="font-mono">out</code>. Wire the upstream node's
	<code class="font-mono">stream</code> handle here and the main flow to
	<code class="font-mono">control</code>.
</p>
