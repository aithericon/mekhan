<script lang="ts">
	/**
	 * config-spec/MapSpecSection.svelte
	 *
	 * Thin wrapper that plugs MAP_SPEC into SchemaDrivenSection and forwards
	 * the full SectionProps contract so the registry dispatcher works without
	 * any changes to NodePropertyPanel.
	 *
	 * Migrated from MapNodeSection.svelte (bespoke RefPicker + two Input elements
	 * + PortsSection) to the spec layer.  Behavior preserved:
	 *   - itemsRef: ref slot with allowArrayBoundary:true; echo line under picker
	 *     (FieldRenderer ref branch renders {#if value} <p font-mono> echo)
	 *   - itemVar: mono text with valueDefault:'item' (live fallback, not placeholder)
	 *   - resultVar: mono text, empty-string fallback
	 *   - output: new 'port' authoring slot — renders PortsSection inline via
	 *     FieldRenderer port branch; default { id:'out', label:'Element', fields:[] }
	 */

	import type { WorkflowNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	import SchemaDrivenSection from './SchemaDrivenSection.svelte';
	import { MAP_SPEC } from '$lib/editor/config-spec/specs';

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

<SchemaDrivenSection spec={MAP_SPEC} {...props} />
