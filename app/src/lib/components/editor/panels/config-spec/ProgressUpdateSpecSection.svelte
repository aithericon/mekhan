<script lang="ts">
	/**
	 * SPIKE — config-spec/ProgressUpdateSpecSection.svelte
	 *
	 * Thin wrapper that plugs PROGRESS_UPDATE_SPEC into SchemaDrivenSection and
	 * forwards the full SectionProps contract so the registry dispatcher works
	 * without any changes to NodePropertyPanel.
	 */

	import type { WorkflowNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	import SchemaDrivenSection from './SchemaDrivenSection.svelte';
	import { PROGRESS_UPDATE_SPEC } from '$lib/editor/config-spec/specs';

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

<SchemaDrivenSection spec={PROGRESS_UPDATE_SPEC} {...props} />
