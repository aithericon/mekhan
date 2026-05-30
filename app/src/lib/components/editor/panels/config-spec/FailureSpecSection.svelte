<script lang="ts">
	/**
	 * config-spec/FailureSpecSection.svelte
	 *
	 * Thin wrapper that plugs FAILURE_SPEC into SchemaDrivenSection and
	 * forwards the full SectionProps contract so the registry dispatcher works
	 * without any changes to NodePropertyPanel.
	 *
	 * Migrated from FailureNodeSection.svelte (bespoke Textarea + mapping list
	 * with RefPicker/InsertRefButton) to the spec layer.  Behavior preserved:
	 *   - failureMessage: textarea with clearToUndefined + InsertRefButton
	 *     (FieldRenderer textarea branch); empty string → undefined coercion
	 *     is explicit in the spec (clearToUndefined:true).
	 *   - errorResultMapping: mapping slot (FieldRenderer mapping branch);
	 *     absent === empty (defaults via ?? []); new-row expression = 'input';
	 *     source = Textarea + RefPicker INSERT helper; per-row Trash + header
	 *     Add hidden when readonly; dashed empty-state; index-keyed; live commit.
	 *   - Trailing advisory folded into failureMessage.description.
	 */

	import type { WorkflowNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	import SchemaDrivenSection from './SchemaDrivenSection.svelte';
	import { FAILURE_SPEC } from '$lib/editor/config-spec/specs';

	type Props = {
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

	let props: Props = $props();
</script>

<SchemaDrivenSection spec={FAILURE_SPEC} {...props} />
