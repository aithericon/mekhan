<script lang="ts">
	// Thin wrapper for the `automated_step` registry entry. An AutomatedStep
	// renders two stacked sections — the backend config + the retry policy —
	// so the registry points at this composite to keep its value type a flat
	// `Record<NodeKind, Component>`.
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import AutomatedStepSection from './AutomatedStepSection.svelte';
	import RetryPolicySection from './RetryPolicySection.svelte';
	import PlacementRequirementsSection from './PlacementRequirementsSection.svelte';

	type Props = {
		data: AutomatedStepNodeData;
		readonly?: boolean;
		onchange: (data: AutomatedStepNodeData) => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
		templateId?: string;
		scope?: ScopeEntry[];
	};

	let {
		data,
		readonly = false,
		onchange,
		binding,
		nodeId,
		templateId,
		scope = []
	}: Props = $props();
</script>

<AutomatedStepSection {data} {readonly} {onchange} {binding} {nodeId} {templateId} {scope} />
<RetryPolicySection {data} {readonly} {onchange} />
<PlacementRequirementsSection {data} {readonly} {onchange} />
