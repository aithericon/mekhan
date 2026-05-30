<script lang="ts">
	/**
	 * SPIKE — config-spec/SchemaDrivenSection.svelte
	 *
	 * Generic renderer for a NodeConfigSpec.  Iterates over spec.fields, reads
	 * each value from `data` by its `bind` key, renders it via FieldRenderer,
	 * and writes back a full updated data object via `onchange` on every field
	 * change.  Keeps zero local state — fully controlled by the parent.
	 *
	 * Props are the full SectionProps contract so wrapper components can spread
	 * their props directly onto this component.
	 */

	import type { NodeConfigSpec } from '$lib/editor/config-spec/types';
	import type { WorkflowNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	import { getByBind, setByBind } from '$lib/editor/config-spec/types';
	import FieldRenderer from './FieldRenderer.svelte';

	type Props = {
		spec: NodeConfigSpec;
		// --- SectionProps contract (subset used here) ---
		data: WorkflowNodeData;
		readonly: boolean;
		onchange: (data: WorkflowNodeData) => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
		templateId?: string;
		scope?: ScopeEntry[];
		resourceScope?: ScopeEntry[];
		onselectnode?: (id: string) => void;
	};

	let {
		spec,
		data,
		readonly,
		onchange,
		scope = [],
		resourceScope = []
	}: Props = $props();

	function handleFieldChange(bind: string, next: unknown) {
		const dataAsRecord = data as unknown as Record<string, unknown>;
		const updated = setByBind(dataAsRecord, bind, next);
		onchange(updated as unknown as WorkflowNodeData);
	}
</script>

<div class="space-y-3">
	{#each spec.fields as fieldSpec (fieldSpec.bind)}
		<FieldRenderer
			spec={fieldSpec}
			value={getByBind(data as unknown as Record<string, unknown>, fieldSpec.bind)}
			data={data as unknown as Record<string, unknown>}
			{scope}
			{resourceScope}
			{readonly}
			onchange={(next) => handleFieldChange(fieldSpec.bind, next)}
		/>
	{/each}
</div>
