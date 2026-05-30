<script lang="ts">
	/**
	 * config-spec/TimeoutSpecSection.svelte
	 *
	 * Thin wrapper that plugs TIMEOUT_SPEC into SchemaDrivenSection and
	 * forwards the full SectionProps contract so the registry dispatcher works
	 * without any changes to NodePropertyPanel.
	 *
	 * Migrated from TimeoutNodeSection.svelte (bespoke GuardEditor + two prose
	 * blocks) to the spec layer.  Behavior preserved: same durationMsExpr field,
	 * same Rhai code editor with RefPicker, same prose folded into description.
	 */

	import type { WorkflowNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	import SchemaDrivenSection from './SchemaDrivenSection.svelte';
	import { TIMEOUT_SPEC } from '$lib/editor/config-spec/specs';

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

<SchemaDrivenSection spec={TIMEOUT_SPEC} {...props} />
