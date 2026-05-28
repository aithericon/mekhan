<script lang="ts">
	// Map: dynamic data-parallel fan-out. At runtime the `itemsRef` array is
	// scattered into one body iteration per element (the element bound to
	// `<itemVar>` on each body token, read by body guards / Python as
	// `<itemVar>.<field>`); the `resultVar` field of each body output is
	// gathered, in array order, into a collection parked on the Map. That
	// collection is borrowable downstream as `<map_slug>[*].<field>` where the
	// `<field>`s are the declared element shape below.
	import type { MapNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';
	import RefPicker from './RefPicker.svelte';
	import PortsSection from './PortsSection.svelte';

	type Port = components['schemas']['Port'];

	type Props = {
		data: MapNodeData;
		readonly?: boolean;
		onchange: (data: MapNodeData) => void;
		/** In-scope producer refs — feeds the `itemsRef` array picker. */
		scope?: ScopeEntry[];
		resourceScope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, scope = [], resourceScope = [] }: Props = $props();

	const itemVar = $derived((data.itemVar ?? 'item').trim() || 'item');
	const element: Port = $derived(data.output ?? { id: 'out', label: 'Element', fields: [] });
</script>

<!--
	The array to scatter. One body iteration per element; pick an array-typed
	upstream field (e.g. `extract.tasks`). The `[*]` iteration boundary is
	implicit — each element is handed to the body as `<itemVar>`.
-->
<FormField label="Items to map over" for="map-items-ref">
	<RefPicker
		{scope}
		{resourceScope}
		selected={data.itemsRef}
		placeholder="Pick an array field…"
		disabled={readonly}
		onpick={(e) => onchange({ ...data, itemsRef: e.qualified })}
	/>
	{#if data.itemsRef}
		<p class="mt-1 font-mono text-sm text-muted-foreground">{data.itemsRef}</p>
	{/if}
</FormField>
<p class="text-sm italic text-muted-foreground">
	The body runs once per element, in array order. A non-array value fails the run.
</p>

<!--
	Element binding. Body guards / Python read `<itemVar>.<field>` for the
	current element. Defaults to `item`.
-->
<FormField label="Element variable" for="map-item-var">
	<Input
		id="map-item-var"
		type="text"
		value={data.itemVar ?? 'item'}
		placeholder="item"
		disabled={readonly}
		oninput={(e) => onchange({ ...data, itemVar: (e.currentTarget as HTMLInputElement).value })}
		class="font-mono"
	/>
</FormField>
<p class="text-sm italic text-muted-foreground">
	Each body iteration reads the current element as
	<code class="font-mono">{itemVar}.&lt;field&gt;</code>.
</p>

<!--
	The field on each body output token whose value becomes the gathered
	element — the per-iteration result the reduce collects.
-->
<FormField label="Collect field" for="map-result-var">
	<Input
		id="map-result-var"
		type="text"
		value={data.resultVar ?? ''}
		placeholder="e.g. score"
		disabled={readonly}
		oninput={(e) => onchange({ ...data, resultVar: (e.currentTarget as HTMLInputElement).value })}
		class="font-mono"
	/>
</FormField>
<p class="text-sm italic text-muted-foreground">
	One value per element, gathered in order into the collection.
</p>

<!--
	Declared shape of one gathered element. These fields are the borrow surface
	downstream: `<map>[*].<field>`. Leave empty for an untyped element.
-->
<PortsSection
	port={element}
	{readonly}
	title="Element shape"
	emptyHint="No element fields declared. The gathered collection borrows as an untyped array — declare fields to expose typed `<map>[*].<field>` refs downstream."
	onchange={(port) => onchange({ ...data, output: port })}
/>
