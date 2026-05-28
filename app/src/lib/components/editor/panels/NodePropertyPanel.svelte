<script lang="ts">
	import type { WorkflowNodeData } from '$lib/types/editor';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import X from '@lucide/svelte/icons/x';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import DerivedPortsSection from './property-sections/DerivedPortsSection.svelte';
	import InScopeRefsSection from './property-sections/InScopeRefsSection.svelte';
	import {
		NODE_PROPERTY_SECTIONS,
		type SectionProps
	} from '$lib/editor/node-property-sections';
	import {
		fetchNodeScopes,
		loadResourceTypes,
		loadWorkspaceResources,
		buildResourceScope,
		type ScopeEntry
	} from '$lib/editor/guard-scope';
	import type { ResourceTypeInfo, ResourceSummary } from '$lib/api/resources';
	import { outputPortsFor } from '$lib/editor/derived-ports';
	import { sanitizeSlug } from '$lib/editor/sanitize-slug';
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

	// In-scope identifiers at the selected node, used by the universal
	// Inputs-in-scope picker as well as every nested section that embeds a
	// RefPicker (Decision, Loop, AutomatedStep, HumanTask). Single source of
	// truth: the backend shape-aware analyzer (`POST /api/analyze`).
	// Debounced so a burst of graph edits collapses to one round-trip;
	// best-effort (stale/empty on failure — never throws).
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
			const result = await fetchNodeScopes(g);
			if (!cancelled) scope = result.scopes.get(id) ?? [];
		}, 250);
		return () => {
			cancelled = true;
			clearTimeout(timer);
		};
	});

	// Mirror the IDE rail: scope visibility is universal for any node type
	// whose authoring depends on what data reaches it. Trigger/Parallel*/
	// Scope/End either produce input or have no per-step refs to insert.
	const SCOPE_BEARING_TYPES = new Set([
		'start',
		'human_task',
		'automated_step',
		'decision',
		'loop',
		'delay',
		'timeout',
		'phase_update',
		'progress_update',
		'failure',
		'end',
		'sub_workflow',
		'agent'
	]);
	const showScopePicker = $derived(SCOPE_BEARING_TYPES.has(data.type));

	// Unmerged fan-in: pure graph topology, computed live so the warning
	// inside the scope panel reflects the current edges.
	const incomingCount = $derived(
		binding && nodeId ? binding.graph.edges.filter((e) => e.target === nodeId).length : 0
	);

	// Workspace resources for the RefPicker's Resources tab. Direct-mode
	// resources are workspace-scoped (no per-workflow alias layer) so the
	// list is the same for every node and every workflow — one
	// module-cached fetch covers the whole session. The type registry is
	// fetched the same way; both run in parallel on mount, the picker
	// degrades to a single-pane mode while either is pending.
	let resourceTypes = $state<ResourceTypeInfo[]>([]);
	let workspaceResources = $state<ResourceSummary[]>([]);
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
	const resourceScope = $derived(buildResourceScope(workspaceResources, resourceTypes));

	// Single exhaustive dispatch: pick the section component for this node kind.
	// Capitalized so the template treats it as a component. The registry's
	// `Record<NodeKind, Component<SectionProps>>` typing makes a missing/spurious
	// kind a compile error in `node-property-sections.ts`.
	const SectionComponent = $derived(NODE_PROPERTY_SECTIONS[data.type]);
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

		<!-- Inputs in scope — universal picker. Canvas omits the manual
		     refresh affordance because the $effect above already debounce-
		     refetches on every graph edit. No `oninsertref` here either:
		     canvas rail has no code editor to target. -->
		{#if showScopePicker}
			<InScopeRefsSection {scope} {resourceScope} {incomingCount} />
		{/if}

		<!-- Type-specific section — single dispatch through the exhaustive
		     `NODE_PROPERTY_SECTIONS` registry (Record<NodeKind, Component>).
		     Every section receives the same `SectionProps` superset and reads
		     only what it needs. A missing/spurious kind is a compile error in
		     `node-property-sections.ts`. -->
		<SectionComponent
			{...{
				data,
				readonly,
				onchange,
				binding,
				nodeId,
				templateId,
				scope,
				resourceScope,
				onselectnode
			} satisfies SectionProps}
		/>

		<!-- Tool tagging (deleted): there's no separate ToolMetaSection
		     anymore. Any node wired to an Agent's `tools` source handle is
		     exposed to the LLM by name; `tool_name` is derived from the
		     node's own `label` (slugified) and `tool_description` from its
		     `description`. Authors just edit the regular per-variant
		     section. The agent's panel lists the wired tools + flags
		     missing labels. -->

		<!-- Phase 4: read-only derived port preview for variants whose outputs
		     come from inner config rather than an editable PortsSection. Start
		     and AutomatedStep already render an editable PortsSection inside
		     their own section. End/Scope have no derived outputs to show
		     until a port editor lands for them. -->
		{#if data.type === 'human_task' || data.type === 'decision' || data.type === 'loop' || data.type === 'delay' || data.type === 'timeout' || data.type === 'parallel_split' || data.type === 'join' || data.type === 'scope' || data.type === 'phase_update' || data.type === 'progress_update' || data.type === 'failure' || data.type === 'agent'}
			<DerivedPortsSection
				ports={outputPortsFor(data)}
				title="Outputs"
				derivedFrom={
					data.type === 'human_task'
						? 'from task fields'
						: data.type === 'decision'
							? 'from branches'
							: data.type === 'agent'
								? 'from agent loop'
								: data.type === 'timeout'
									? 'done + timeout'
									: 'pass-through'
				}
			/>
		{/if}
	</div>
</div>
