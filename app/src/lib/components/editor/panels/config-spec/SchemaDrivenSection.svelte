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
		binding,
		nodeId,
		templateId,
		scope = [],
		resourceScope = [],
		onselectnode
	}: Props = $props();

	function handleFieldChange(bind: string, next: unknown) {
		const dataAsRecord = data as unknown as Record<string, unknown>;
		const updated = setByBind(dataAsRecord, bind, next);
		onchange(updated as unknown as WorkflowNodeData);
	}
</script>

<div class="space-y-3">
	{#each spec.fields as fieldSpec (fieldSpec.kind === 'custom' ? `custom:${(fieldSpec as { component: string }).component}` : (fieldSpec as { bind: string }).bind)}
		<FieldRenderer
			spec={fieldSpec}
			value={fieldSpec.kind === 'custom' ? undefined : getByBind(data as unknown as Record<string, unknown>, (fieldSpec as { bind: string }).bind)}
			data={data as unknown as Record<string, unknown>}
			{scope}
			{resourceScope}
			{readonly}
			{binding}
			{nodeId}
			{templateId}
			{onselectnode}
			onchange={(next) => {
				if (fieldSpec.kind === 'custom') return;
				handleFieldChange((fieldSpec as { bind: string }).bind, next);
			}}
		/>
	{/each}
</div>
