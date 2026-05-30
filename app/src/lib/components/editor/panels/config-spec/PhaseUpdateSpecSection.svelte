<script lang="ts">
	/**
	 * config-spec/PhaseUpdateSpecSection.svelte
	 *
	 * Thin wrapper that plugs PHASE_UPDATE_SPEC into SchemaDrivenSection and
	 * forwards the full SectionProps contract so the registry dispatcher works
	 * without any changes to NodePropertyPanel.
	 *
	 * Migrated from PhaseUpdateNodeSection.svelte (bespoke Input + Select + Textarea
	 * with InsertRefButton blocks) to the spec layer.  Behavior preserved:
	 *   - phaseName: text field with InsertRefButton (FieldRenderer text branch)
	 *   - status: select with displayDefault:'running' (shown when undefined,
	 *     NOT written until user picks — FieldRenderer select branch guards write)
	 *   - message: textarea with clearToUndefined + InsertRefButton
	 *   - trailing advisory folded into message.description
	 */

	import type { WorkflowNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	import SchemaDrivenSection from './SchemaDrivenSection.svelte';
	import { PHASE_UPDATE_SPEC } from '$lib/editor/config-spec/specs';

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

<SchemaDrivenSection spec={PHASE_UPDATE_SPEC} {...props} />
