<script lang="ts">
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { WorkflowNodeData } from '$lib/types/editor';
	import StartNodeSection from '$lib/components/editor/panels/property-sections/StartNodeSection.svelte';
	import HumanTaskSection from '$lib/components/editor/panels/property-sections/HumanTaskSection.svelte';
	import AutomatedStepSection from '$lib/components/editor/panels/property-sections/AutomatedStepSection.svelte';
	import DecisionNodeSection from '$lib/components/editor/panels/property-sections/DecisionNodeSection.svelte';
	import LoopNodeSection from '$lib/components/editor/panels/property-sections/LoopNodeSection.svelte';

	type Props = {
		binding: YjsGraphBinding;
		nodeId: string;
		readonly?: boolean;
	};

	let { binding, nodeId, readonly = false }: Props = $props();

	const nodeData = $derived(
		binding.graph.nodes.find((n) => n.id === nodeId)?.data ?? null
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
		<span class="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Node Config</span>
	</div>

	{#if nodeData}
		<div class="flex-1 space-y-4 overflow-y-auto p-3">
			<div class="space-y-1.5">
				<label for="ide-node-label" class="text-xs font-medium text-muted-foreground">Label</label>
				<input
					id="ide-node-label"
					type="text"
					value={nodeData.label}
					disabled={readonly}
					oninput={(e) => updateField('label', (e.currentTarget as HTMLInputElement).value)}
					class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
				/>
			</div>

			<div class="space-y-1.5">
				<label for="ide-node-desc" class="text-xs font-medium text-muted-foreground">Description</label>
				<textarea
					id="ide-node-desc"
					value={nodeData.description ?? ''}
					disabled={readonly}
					oninput={(e) => updateField('description', (e.currentTarget as HTMLTextAreaElement).value)}
					rows={2}
					class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
				></textarea>
			</div>

			{#if nodeData.type === 'start'}
				<StartNodeSection data={nodeData} {readonly} onchange={handleChange} />
			{:else if nodeData.type === 'human_task'}
				<div class="rounded-lg border border-dashed border-border bg-muted/20 p-3">
					<p class="text-xs text-muted-foreground">
						Task form is edited in the center panel
					</p>
				</div>
			{:else if nodeData.type === 'automated_step'}
				<AutomatedStepSection data={nodeData} {readonly} onchange={handleChange} {binding} {nodeId} />
			{:else if nodeData.type === 'decision'}
				<DecisionNodeSection data={nodeData} {readonly} onchange={handleChange} />
			{:else if nodeData.type === 'loop'}
				<LoopNodeSection data={nodeData} {readonly} onchange={handleChange} />
			{/if}
		</div>
	{:else}
		<div class="flex flex-1 items-center justify-center text-sm text-muted-foreground">
			Select a node from the file tree
		</div>
	{/if}
</div>
