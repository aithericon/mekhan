<script lang="ts">
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { WorkflowNodeData, HumanTaskNodeData } from '$lib/types/editor';
	import HumanTaskSection from '$lib/components/editor/panels/property-sections/HumanTaskSection.svelte';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';

	type Props = {
		binding: YjsGraphBinding;
		nodeId: string;
		readonly?: boolean;
	};

	let { binding, nodeId, readonly = false }: Props = $props();

	const nodeData = $derived(
		binding.graph.nodes.find((n) => n.id === nodeId)?.data as HumanTaskNodeData | null
	);

	function handleChange(data: WorkflowNodeData) {
		binding.updateNodeData(nodeId, data);
	}
</script>

<div class="flex h-full flex-col">
	<div class="flex items-center border-b border-border bg-card px-4 py-2">
		<span class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
			Task Form Editor
		</span>
		{#if nodeData}
			<span class="ml-2 text-xs text-muted-foreground">— {nodeData.label}</span>
		{/if}
	</div>

	{#if nodeData}
		<div class="flex-1 overflow-y-auto p-6">
			<div class="mx-auto max-w-2xl space-y-5">
				<HumanTaskSection
					data={nodeData}
					{readonly}
					onchange={handleChange}
				/>
			</div>
		</div>
	{:else}
		<div class="flex flex-1 items-center justify-center text-sm text-muted-foreground">
			Node not found
		</div>
	{/if}
</div>
