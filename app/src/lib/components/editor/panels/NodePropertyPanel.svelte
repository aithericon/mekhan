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
	import FailureNodeSection from './property-sections/FailureNodeSection.svelte';
	import EndNodeSection from './property-sections/EndNodeSection.svelte';
	import { fetchNodeScopes, type ScopeEntry } from '$lib/editor/guard-scope';
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

	// ── Node slug — the author-facing `<slug>.<field>` guard namespace. It is
	//    a node-level prop (not in `data`), so it round-trips through the Yjs
	//    binding. Blank ⇒ the compiler derives a deterministic default from
	//    the node id (shown as the placeholder).
	function sanitizeSlug(raw: string): string {
		const s = raw
			.trim()
			.toLowerCase()
			.replace(/[^a-z0-9_]+/g, '_')
			.replace(/_+/g, '_')
			.replace(/^_+|_+$/g, '');
		if (!s) return 'node';
		return /^[a-z]/.test(s) ? s : `n_${s}`;
	}

	const currentNode = $derived(binding?.graph.nodes.find((n) => n.id === nodeId));
	const slugValue = $derived(currentNode?.slug ?? '');
	const slugPlaceholder = $derived(nodeId ? sanitizeSlug(nodeId) : 'slug');
	// Inline validation: Rhai-identifier-safe + unique across the graph.
	const slugError = $derived.by(() => {
		const v = slugValue.trim();
		if (!v) return null; // empty ⇒ derived default, always valid
		if (!/^[a-z][a-z0-9_]*$/.test(v))
			return 'Lowercase letter, then letters/digits/underscore (e.g. review_step).';
		const clash = binding?.graph.nodes.some(
			(n) => n.id !== nodeId && (n.slug ?? '').trim() === v
		);
		return clash ? `Slug "${v}" is already used by another node.` : null;
	});

	function updateSlug(value: string) {
		if (binding && nodeId) binding.updateNodeSlug(nodeId, value);
	}

	// In-scope identifiers at the selected node for the Decision/Loop guard
	// pickers. Single source of truth: the backend shape-aware analyzer
	// (`POST /api/analyze`). Debounced so a burst of graph edits collapses to
	// one round-trip; best-effort (stale/empty on failure — never throws).
	let scope = $state<ScopeEntry[]>([]);
	$effect(() => {
		const g = binding?.graph;
		const id = nodeId;
		if (!g || !id) {
			scope = [];
			return;
		}
		let cancelled = false;
		const timer = setTimeout(async () => {
			const all = await fetchNodeScopes(g);
			if (!cancelled) scope = all.get(id) ?? [];
		}, 250);
		return () => {
			cancelled = true;
			clearTimeout(timer);
		};
	});
</script>

<div
	class="flex w-[480px] shrink-0 flex-col border-l border-border bg-card"
	data-testid="node-property-panel"
>
	<div class="flex items-center justify-between border-b border-border px-3 py-2.5">
		<h2 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground">
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

		<!-- Common: Slug — author-facing `<slug>.<field>` guard namespace.
		     Node-level, so it needs the Yjs binding. -->
		{#if binding && nodeId}
			<FormField label="Slug" for="node-slug">
				<Input
					id="node-slug"
					type="text"
					value={slugValue}
					placeholder={slugPlaceholder}
					disabled={readonly}
					aria-invalid={slugError ? 'true' : undefined}
					data-testid="input-node-slug"
					oninput={(e) => updateSlug((e.currentTarget as HTMLInputElement).value)}
				/>
				{#if slugError}
					<p class="mt-1 text-sm text-destructive" data-testid="node-slug-error">
						{slugError}
					</p>
				{:else}
					<p class="mt-1 text-sm text-muted-foreground">
						Referenced in guards as <code>{(slugValue.trim() || slugPlaceholder)}.&lt;field&gt;</code>.
						Blank derives from the node id.
					</p>
				{/if}
			</FormField>
		{/if}

		<!-- Type-specific sections -->
		{#if data.type === 'start'}
			<StartNodeSection {data} {readonly} {onchange} {binding} {nodeId} {onselectnode} />
		{:else if data.type === 'human_task'}
			{#if templateId && nodeId}
				<div class="space-y-3">
					<div class="rounded-lg border border-border bg-muted/30 p-3">
						<p class="text-sm text-muted-foreground">
							{data.steps.length} step{data.steps.length !== 1 ? 's' : ''} configured
						</p>
						{#if data.taskTitle}
							<p class="mt-1 truncate text-sm font-medium text-foreground">{data.taskTitle}</p>
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
		{:else if data.type === 'failure'}
			<FailureNodeSection {data} {readonly} {onchange} />
		{:else if data.type === 'end'}
			<EndNodeSection {data} {readonly} {onchange} />
		{/if}

		<!-- Phase 4: read-only derived port preview for variants whose outputs
		     come from inner config rather than an editable PortsSection. Start
		     and AutomatedStep already render an editable PortsSection inside
		     their own section. End/Scope have no derived outputs to show
		     until a port editor lands for them. -->
		{#if data.type === 'human_task' || data.type === 'decision' || data.type === 'loop' || data.type === 'parallel_split' || data.type === 'parallel_join' || data.type === 'scope' || data.type === 'phase_update' || data.type === 'progress_update' || data.type === 'failure'}
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
