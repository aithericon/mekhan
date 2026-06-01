<script lang="ts">
	// StreamFold: drains the stream side-channel from an upstream AutomatedStep
	// with `streamOutput: true` and folds the chunks into a single output token
	// via the selected reduce strategy. No body — the fold is pure Rhai.
	import type { StreamFoldNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import { FormField } from '$lib/components/ui/form-field';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import * as Select from '$lib/components/ui/select';

	type StreamReduce = components['schemas']['StreamReduce'];

	type Props = {
		data: StreamFoldNodeData;
		readonly?: boolean;
		onchange: (data: StreamFoldNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();

	const reduce = $derived((data.reduce ?? { kind: 'array' }) as StreamReduce);
	const reduceKind = $derived(reduce.kind);

	const kindLabels: Record<StreamReduce['kind'], string> = {
		array: 'Array (ordered list of chunks)',
		concat: 'Concat (join into a string)',
		sum: 'Sum (numeric total)',
		custom: 'Custom (Rhai expression)'
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
</script>

<!--
	Result variable: the field name the reduced result is projected onto.
	The compiler reads it to name the parked output (`<slug>.<resultVar>`).
-->
<FormField label="Result variable" for="sf-result-var">
	<Input
		id="sf-result-var"
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
	Reduce strategy: how the drained chunks are folded into the single output
	token.
-->
<FormField label="Reduce strategy" for="sf-reduce-kind">
	<Select.Root
		type="single"
		value={reduceKind}
		onValueChange={(v) => {
			if (v) setKind(v as StreamReduce['kind']);
		}}
		disabled={readonly}
	>
		<Select.Trigger id="sf-reduce-kind" class="w-full" disabled={readonly}>
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
	<FormField label="Separator (optional)" for="sf-sep">
		<Input
			id="sf-sep"
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
	<FormField label="Rhai expression" for="sf-expr">
		<Textarea
			id="sf-expr"
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

<p class="text-sm italic text-muted-foreground">
	The fold drains all chunks from the stream handle before emitting via
	<code class="font-mono">out</code>. Wire the upstream node's
	<code class="font-mono">stream</code> handle here and the main flow to
	<code class="font-mono">control</code>.
</p>
