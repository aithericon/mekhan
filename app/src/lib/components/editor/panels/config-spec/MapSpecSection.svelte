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
	import type { MapNodeData } from '$lib/types/editor';

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

	// Source toggle: a STREAMING Map ingests a producer's chunks (stream/control
	// edges) instead of scattering a static `itemsRef` array. `itemsRef` is
	// ignored when streaming.
	const streamSource = $derived((props.data as MapNodeData).streamSource ?? false);
	function toggleStreamSource(e: Event) {
		const checked = (e.target as HTMLInputElement).checked;
		props.onchange({ ...props.data, streamSource: checked } as WorkflowNodeData);
	}
</script>

<div class="space-y-1 pb-3 mb-3 border-b border-border/40">
	<label class="flex items-center gap-2 text-sm">
		<input
			type="checkbox"
			checked={streamSource}
			disabled={props.readonly}
			onchange={toggleStreamSource}
		/>
		<span>Stream source</span>
	</label>
	<p class="text-sm text-muted-foreground">
		Ingest a streaming producer's chunks (wire its <code class="font-mono">stream</code> handle
		to this map's <code class="font-mono">stream</code> and its <code class="font-mono">out</code>
		to <code class="font-mono">control</code>) instead of scattering
		<code class="font-mono">itemsRef</code>. Parallel-only; sized on the runtime stream count.
		<code class="font-mono">itemsRef</code> is ignored when enabled.
	</p>
</div>

<SchemaDrivenSection spec={MAP_SPEC} {...props} />
