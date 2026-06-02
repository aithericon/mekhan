<script lang="ts">
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { WorkflowNodeData } from '$lib/types/editor';
	import StartNodeSection from '$lib/components/editor/panels/property-sections/StartNodeSection.svelte';
	import HumanTaskSection from '$lib/components/editor/panels/property-sections/HumanTaskSection.svelte';
	import AutomatedStepSection from '$lib/components/editor/panels/property-sections/AutomatedStepSection.svelte';
	import DecisionNodeSection from '$lib/components/editor/panels/property-sections/DecisionNodeSection.svelte';
	import LoopNodeSection from '$lib/components/editor/panels/property-sections/LoopNodeSection.svelte';
	import DelayNodeSection from '$lib/components/editor/panels/property-sections/DelayNodeSection.svelte';
	import TimeoutNodeSection from '$lib/components/editor/panels/property-sections/TimeoutNodeSection.svelte';
	import InScopeRefsSection from '$lib/components/editor/panels/property-sections/InScopeRefsSection.svelte';
	import {
		loadResourceTypes,
		loadWorkspaceResources,
		loadTemplateAssetScope,
		buildResourceScope,
		type ScopeEntry
	} from '$lib/editor/guard-scope';
	import type { ResourceTypeInfo, ResourceSummary } from '$lib/api/resources';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';

	type Props = {
		binding: YjsGraphBinding;
		nodeId: string;
		readonly?: boolean;
		/** This node's in-scope refs (from `/api/v1/analyze`). Drives the
		 *  Inputs-in-scope picker and every nested section that embeds a
		 *  RefPicker (Decision, Loop, AutomatedStep, HumanTask). */
		scope?: ScopeEntry[];
		/** Template this node belongs to. Drives the Assets tab of RefPicker:
		 *  the picker lists every template-visible asset so the user can browse
		 *  and select from their asset library. */
		templateId?: string | null;
		scopeBusy?: boolean;
		onRefreshScope?: () => void;
		/** Insert a snippet at the active code editor's cursor. When
		 *  undefined, the scope picker renders refs as static text. */
		oninsertref?: (snippet: string) => void;
	};

	let {
		binding,
		nodeId,
		readonly = false,
		scope = [],
		templateId = null,
		scopeBusy = false,
		onRefreshScope,
		oninsertref
	}: Props = $props();

	// The RefPicker is a LIBRARY BROWSER: its Resources and Assets tabs list
	// everything the user can select — the full workspace resource set and the
	// full template-visible asset set — not only what the graph references. We
	// load both libraries client-side and feed their union to the picker.
	let resourceTypes = $state<ResourceTypeInfo[]>([]);
	let workspaceResources = $state<ResourceSummary[]>([]);
	let assetScope = $state<ScopeEntry[]>([]);
	$effect(() => {
		void loadResourceTypes()
			.then((types) => {
				resourceTypes = types;
			})
			.catch(() => {});
		void loadWorkspaceResources()
			.then((items) => {
				workspaceResources = items;
			})
			.catch(() => {});
	});
	$effect(() => {
		const tid = templateId;
		if (!tid) {
			assetScope = [];
			return;
		}
		let cancelled = false;
		void loadTemplateAssetScope(tid)
			.then((entries) => {
				if (!cancelled) assetScope = entries;
			})
			.catch(() => {});
		return () => {
			cancelled = true;
		};
	});
	const resourceScope = $derived([
		...buildResourceScope(workspaceResources, resourceTypes),
		...assetScope
	]);

	const nodeData = $derived(
		binding.graph.nodes.find((n) => n.id === nodeId)?.data ?? null
	);

	// Node types whose authoring depends on what data is in scope. Everything
	// else (Trigger, ParallelSplit/Join, Scope, End) either produces input or
	// has no per-step references to insert, so suppress the picker.
	const SCOPE_BEARING_TYPES = new Set([
		'start',
		'human_task',
		'automated_step',
		'decision',
		'loop',
		'phase_update',
		'progress_update',
		'failure',
		'sub_workflow'
	]);
	const showScopePicker = $derived(
		nodeData ? SCOPE_BEARING_TYPES.has(nodeData.type) : false
	);

	// Unmerged fan-in: >1 edge into a non-Join step means it runs once per
	// upstream token (no merge) and the scope below is the union across
	// branches, not one run's data. Pure graph topology — compute live.
	const incomingCount = $derived(
		binding.graph.edges.filter((e) => e.target === nodeId).length
	);

	function handleChange(data: WorkflowNodeData) {
		binding.updateNodeData(nodeId, data);
	}

	function updateField<K extends keyof WorkflowNodeData>(
		key: K,
		value: WorkflowNodeData[K]
	) {
		if (!nodeData) return;
		handleChange({ ...nodeData, [key]: value } as WorkflowNodeData);
	}
</script>

<div class="flex h-full flex-col border-l border-border bg-card">
	<div class="border-b border-border px-3 py-2">
		<span class="text-sm font-semibold uppercase tracking-wider text-muted-foreground">Node Config</span>
	</div>

	{#if nodeData}
		<div class="flex-1 space-y-4 overflow-y-auto p-3">
			<div class="space-y-1.5">
				<label for="ide-node-label" class="text-sm font-medium text-muted-foreground">Label</label>
				<Input
					id="ide-node-label"
					type="text"
					value={nodeData.label}
					disabled={readonly}
					oninput={(e) => updateField('label', (e.currentTarget as HTMLInputElement).value)}
				/>
			</div>

			<div class="space-y-1.5">
				<label for="ide-node-desc" class="text-sm font-medium text-muted-foreground">Description</label>
				<Textarea
					id="ide-node-desc"
					value={nodeData.description ?? ''}
					disabled={readonly}
					oninput={(e) => updateField('description', (e.currentTarget as HTMLTextAreaElement).value)}
					rows={2}
				/>
			</div>

			{#if showScopePicker}
				<InScopeRefsSection
					{scope}
					{resourceScope}
					busy={scopeBusy}
					{incomingCount}
					onRefresh={onRefreshScope}
					{oninsertref}
				/>
			{/if}

			{#if nodeData.type === 'start'}
				<StartNodeSection data={nodeData} {readonly} onchange={handleChange} {scope} />
			{:else if nodeData.type === 'human_task'}
				<div class="rounded-lg border border-dashed border-border bg-muted/20 p-3">
					<p class="text-sm text-muted-foreground">
						Task form is edited in the center panel
					</p>
				</div>
			{:else if nodeData.type === 'automated_step'}
				<AutomatedStepSection data={nodeData} {readonly} onchange={handleChange} {binding} {nodeId} {scope} />
			{:else if nodeData.type === 'decision'}
				<DecisionNodeSection data={nodeData} {readonly} onchange={handleChange} {scope} {resourceScope} />
			{:else if nodeData.type === 'loop'}
				<LoopNodeSection data={nodeData} {readonly} onchange={handleChange} {scope} {resourceScope} />
			{:else if nodeData.type === 'delay'}
				<DelayNodeSection data={nodeData} {readonly} onchange={handleChange} {scope} {resourceScope} />
			{:else if nodeData.type === 'timeout'}
				<TimeoutNodeSection data={nodeData} {readonly} onchange={handleChange} {scope} {resourceScope} />
			{/if}
		</div>
	{:else}
		<div class="flex flex-1 items-center justify-center text-sm text-muted-foreground">
			Select a node from the file tree
		</div>
	{/if}
</div>
