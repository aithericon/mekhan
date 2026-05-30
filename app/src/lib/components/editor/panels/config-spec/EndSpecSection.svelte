<script lang="ts">
	/**
	 * config-spec/EndSpecSection.svelte
	 *
	 * Thin wrapper that plugs END_SPEC into SchemaDrivenSection and forwards
	 * the full SectionProps contract so the registry dispatcher works without
	 * any changes to NodePropertyPanel.
	 *
	 * Migrated from EndNodeSection.svelte (bespoke RefPicker + FieldMapping[]
	 * list editor) to the spec layer. Behavior preserved:
	 *   - resultMapping: mapping slot with source.widget:'refpicker' and
	 *     autoFillTargetWhenBlank:true (FieldRenderer mapping branch, refpicker
	 *     variant). Same testids: btn-add-result-mapping, input-result-target.
	 *     Auto-fill targetField from picked field name when blank (rename-preserve).
	 *     Per-row RefPicker + Input + Trash; dashed empty-state; verbatim write-back.
	 *   - Footer prose rendered after the list via MappingField.footer.
	 *   - Empty-state hint: plain text (the <code> tag in the original is lost —
	 *     minor cosmetic regression accepted per design).
	 */

	import type { WorkflowNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	import SchemaDrivenSection from './SchemaDrivenSection.svelte';
	import { END_SPEC } from '$lib/editor/config-spec/specs';

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

<SchemaDrivenSection spec={END_SPEC} {...props} />
