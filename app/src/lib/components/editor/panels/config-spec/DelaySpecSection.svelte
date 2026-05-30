<script lang="ts">
	/**
	 * SPIKE — config-spec/DelaySpecSection.svelte
	 *
	 * Thin wrapper that plugs the DELAY_SPEC into SchemaDrivenSection and
	 * forwards the full SectionProps contract so the registry dispatcher works
	 * without any changes to NodePropertyPanel.
	 */

	import type { WorkflowNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	import SchemaDrivenSection from './SchemaDrivenSection.svelte';
	import { DELAY_SPEC } from '$lib/editor/config-spec/specs';

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

<SchemaDrivenSection spec={DELAY_SPEC} {...props} />
