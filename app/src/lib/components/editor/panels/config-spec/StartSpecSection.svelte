<script lang="ts">
	/**
	 * config-spec/StartSpecSection.svelte
	 *
	 * Thin wrapper that plugs START_SPEC into SchemaDrivenSection and forwards
	 * the full SectionProps contract so the registry dispatcher works without
	 * any changes to NodePropertyPanel.
	 *
	 * Migrated from StartNodeSection.svelte (bespoke Input + PortsSection +
	 * graph-relational Entrypoints) to the spec layer. Behavior preserved:
	 *   - processName: text field with clearToNull:true ('' → null, matching the
	 *     bespoke coercion: value.length ? value : null) and InsertRefButton when
	 *     scope.length > 0 (text branch already wires this in FieldRenderer).
	 *   - initial: port slot with default { id:'in', label:'Input', fields:[] };
	 *     verbatim write-back; id:'in' preserved so trigger edges' targetHandle
	 *     pins correctly.
	 *   - Entrypoints: custom slot 'start.entrypoints' → StartEntrypoints.svelte.
	 *     Receives full section context (binding, nodeId, onselectnode).
	 *     Testids preserved: start-entrypoints, btn-add-trigger, trigger-link.
	 */

	import type { WorkflowNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	import SchemaDrivenSection from './SchemaDrivenSection.svelte';
	import { START_SPEC } from '$lib/editor/config-spec/specs';

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

<SchemaDrivenSection spec={START_SPEC} {...props} />
