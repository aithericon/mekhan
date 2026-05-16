<script lang="ts">
	import type { WorkflowNodeData } from '$lib/types/editor';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import X from '@lucide/svelte/icons/x';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import StartNodeSection from './property-sections/StartNodeSection.svelte';
	import HumanTaskSection from './property-sections/HumanTaskSection.svelte';
	import AutomatedStepSection from './property-sections/AutomatedStepSection.svelte';
	import DecisionNodeSection from './property-sections/DecisionNodeSection.svelte';
	import LoopNodeSection from './property-sections/LoopNodeSection.svelte';
	import DerivedPortsSection from './property-sections/DerivedPortsSection.svelte';
	import TriggerNodeSection from './property-sections/TriggerNodeSection.svelte';
	import RetryPolicySection from './property-sections/RetryPolicySection.svelte';
	import ParallelSplitSection from './property-sections/ParallelSplitSection.svelte';
	import ParallelJoinSection from './property-sections/ParallelJoinSection.svelte';
	import ScopeSection from './property-sections/ScopeSection.svelte';
	import PhaseUpdateNodeSection from './property-sections/PhaseUpdateNodeSection.svelte';
	import ProgressUpdateNodeSection from './property-sections/ProgressUpdateNodeSection.svelte';
	import { computeScopes, type ScopeEntry } from '$lib/editor/guard-scope';
	import { outputPortsFor } from '$lib/editor/derived-ports';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';

	type Props = {
		data: WorkflowNodeData;
		readonly?: boolean;
		onchange: (data: WorkflowNodeData) => void;
		onclose: () => void;
		ondelete?: () => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
		templateId?: string;
		/// Select a different node by id (swaps the property panel to it).
		/// Used by the Start section's "Add trigger" affordance to jump to
		/// the freshly-created Trigger node.
		onselectnode?: (id: string) => void;
	};

	let {
		data,
		readonly = false,
		onchange,
		onclose,
		ondelete,
		binding,
		nodeId,
		templateId,
		onselectnode
	}: Props = $props();

	function updateField<K extends keyof WorkflowNodeData>(
		key: K,
		value: WorkflowNodeData[K]
	) {
		onchange({ ...data, [key]: value } as WorkflowNodeData);
	}

	// Compute the in-scope identifiers at the currently-selected node so the
	// Decision/Loop guard editors can offer typed pickers + autocomplete. We
	// re-run on every change to `binding.graph` (cheap — O(nodes + edges) and
	// the editor is already paying for full Y.Doc rerenders).
	const scope: ScopeEntry[] = $derived.by(() => {
		if (!binding || !nodeId) return [];
		const all = computeScopes(binding.graph);
		return all.get(nodeId) ?? [];
	});
</script>

<div
	class="flex w-[480px] shrink-0 flex-col border-l border-border bg-card"
	data-testid="node-property-panel"
>
	<div class="flex items-center justify-between border-b border-border px-3 py-2.5">
		<h2 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
			{readonly ? 'Inspector' : 'Properties'}
		</h2>
		<div class="flex items-center gap-0.5">
			{#if !readonly && ondelete}
				<button
					type="button"
					class="rounded-md p-1 text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive"
					data-testid="btn-delete-node"
					onclick={ondelete}
					title="Delete node"
					aria-label="Delete node"
				>
					<Trash2 class="size-4" />
				</button>
			{/if}
			<button
				type="button"
				class="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
				data-testid="btn-close-properties"
				onclick={onclose}
			>
				<X class="size-4" />
			</button>
		</div>
	</div>

	<div class="flex-1 space-y-4 overflow-y-auto p-3">
		<!-- Common: Label -->
		<FormField label="Label" for="node-label">
			<Input
				id="node-label"
				type="text"
				value={data.label}
				disabled={readonly}
				data-testid="input-node-label"
				oninput={(e) => updateField('label', (e.currentTarget as HTMLInputElement).value)}
			/>
		</FormField>

		<!-- Common: Description -->
		<FormField label="Description" for="node-desc">
			<Textarea
				id="node-desc"
				value={data.description ?? ''}
				disabled={readonly}
				data-testid="input-node-description"
				oninput={(e) => updateField('description', (e.currentTarget as HTMLTextAreaElement).value)}
				rows={2}
			/>
		</FormField>

		<!-- Type-specific sections -->
		{#if data.type === 'start'}
			<StartNodeSection {data} {readonly} {onchange} {binding} {nodeId} {onselectnode} />
		{:else if data.type === 'human_task'}
			{#if templateId && nodeId}
				<div class="space-y-3">
					<div class="rounded-lg border border-border bg-muted/30 p-3">
						<p class="text-xs text-muted-foreground">
							{data.steps.length} step{data.steps.length !== 1 ? 's' : ''} configured
						</p>
						{#if data.taskTitle}
							<p class="mt-1 truncate text-xs font-medium text-foreground">{data.taskTitle}</p>
						{/if}
					</div>
					<Button
						variant="outline"
						size="sm"
						class="w-full"
						href="/templates/{templateId}/ide?node={nodeId}"
					>
						<Pencil class="size-3.5" />
						Edit Task Form
					</Button>
				</div>
			{:else}
				<HumanTaskSection {data} {readonly} {onchange} />
			{/if}
		{:else if data.type === 'automated_step'}
			<AutomatedStepSection {data} {readonly} {onchange} {binding} {nodeId} {templateId} />
			<RetryPolicySection {data} {readonly} {onchange} />
		{:else if data.type === 'decision'}
			<DecisionNodeSection {data} {readonly} {onchange} {scope} />
		{:else if data.type === 'loop'}
			<LoopNodeSection {data} {readonly} {onchange} {scope} />
		{:else if data.type === 'trigger'}
			<TriggerNodeSection {data} {readonly} {onchange} {nodeId} {binding} />
		{:else if data.type === 'parallel_split'}
			<ParallelSplitSection {data} {binding} {nodeId} />
		{:else if data.type === 'parallel_join'}
			<ParallelJoinSection {data} {readonly} {onchange} {binding} {nodeId} />
		{:else if data.type === 'scope'}
			<ScopeSection {data} {binding} {nodeId} />
		{:else if data.type === 'phase_update'}
			<PhaseUpdateNodeSection {data} {readonly} {onchange} />
		{:else if data.type === 'progress_update'}
			<ProgressUpdateNodeSection {data} {readonly} {onchange} />
		{/if}

		<!-- Phase 4: read-only derived port preview for variants whose outputs
		     come from inner config rather than an editable PortsSection. Start
		     and AutomatedStep already render an editable PortsSection inside
		     their own section. End/Scope have no derived outputs to show
		     until a port editor lands for them. -->
		{#if data.type === 'human_task' || data.type === 'decision' || data.type === 'loop' || data.type === 'parallel_split' || data.type === 'parallel_join' || data.type === 'scope' || data.type === 'phase_update' || data.type === 'progress_update'}
			<DerivedPortsSection
				ports={outputPortsFor(data)}
				title="Outputs"
				derivedFrom={
					data.type === 'human_task'
						? 'from task fields'
						: data.type === 'decision'
							? 'from branches'
							: 'pass-through'
				}
			/>
		{/if}
	</div>
</div>
