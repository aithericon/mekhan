<script lang="ts">
	import { createDefaultNodeData, type StartNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { Button } from '$lib/components/ui/button';
	import PortsSection from './PortsSection.svelte';
	import InsertRefButton from './InsertRefButton.svelte';
	import Zap from '@lucide/svelte/icons/zap';
	import Plus from '@lucide/svelte/icons/plus';

	type Port = components['schemas']['Port'];

	type Props = {
		data: StartNodeData;
		readonly?: boolean;
		onchange: (data: StartNodeData) => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
		scope?: ScopeEntry[];
		onselectnode?: (id: string) => void;
	};

	let {
		data,
		readonly = false,
		onchange,
		binding,
		nodeId,
		scope = [],
		onselectnode
	}: Props = $props();

	function appendToProcessName(snippet: string) {
		const curr = data.processName ?? '';
		handleProcessNameChange(curr ? `${curr} ${snippet}` : snippet);
	}

	// Pre-typed-ports templates have no `initial` field — synthesize an empty
	// input port so the editor renders cleanly.
	const initial: Port = $derived(
		data.initial ?? { id: 'in', label: 'Input', fields: [] }
	);

	function handlePortChange(port: Port) {
		onchange({ ...data, initial: port });
	}

	function handleProcessNameChange(value: string) {
		// Empty = opt out: no named process is registered for instances.
		onchange({ ...data, processName: value.length ? value : null });
	}

	const sourceKindLabels: Record<string, string> = {
		manual: 'API call',
		cron: 'Cron schedule',
		catalog: 'Catalogue event',
		net_completion: 'On workflow completion',
		webhook: 'Webhook'
	};

	// Triggers are standalone nodes, not part of this Start's data — surface
	// the ones wired into this Start so authors don't have to hunt the canvas
	// to discover (or attach) the workflow's entrypoints. A trigger feeds this
	// Start when its single outgoing edge targets this node.
	const feedingTriggers = $derived.by(() => {
		if (!binding || !nodeId) return [];
		const g = binding.graph;
		const out: { id: string; label: string; kind: string; enabled: boolean }[] = [];
		for (const n of g.nodes) {
			if (n.data.type !== 'trigger') continue;
			const edge = g.edges.find((e) => e.source === n.id);
			if (edge && edge.target === nodeId) {
				out.push({
					id: n.id,
					label: n.data.label,
					kind: n.data.source?.kind ?? 'manual',
					enabled: n.data.enabled ?? false
				});
			}
		}
		return out;
	});

	const canEditGraph = $derived(!readonly && !!binding && !!nodeId);

	function addTrigger() {
		if (!binding || !nodeId || readonly) return;
		const startNode = binding.graph.nodes.find((n) => n.id === nodeId);
		const base = startNode?.position ?? { x: 0, y: 0 };
		// Sit the trigger to the left of the Start (its `out` source handle is
		// on the right, the Start's `target` handle on the left), stacked so
		// multiple triggers don't overlap.
		const position = { x: base.x - 260, y: base.y + feedingTriggers.length * 88 };
		const triggerId = `node-${Date.now()}`;
		const triggerData = createDefaultNodeData('trigger');
		binding.addNode(triggerId, 'trigger', position, triggerData);
		binding.addEdge({
			id: `e-${triggerId}-${nodeId}-${Date.now()}`,
			source: triggerId,
			target: nodeId,
			sourceHandle: 'out',
			// Must equal the Start's `initial` port id — the dispatcher and
			// validate_triggers resolve a Start target via output_ports() and
			// match on this handle (see docs/06-triggers.md).
			targetHandle: initial.id,
			type: 'sequence'
		});
		// Jump straight into the new trigger's config — the point of the
		// affordance is to configure how the workflow fires, not just to
		// drop a node.
		onselectnode?.(triggerId);
	}
</script>

<div class="space-y-2">
	<Label for="process-name">Process name</Label>
	<Input
		id="process-name"
		type="text"
		placeholder="e.g. Invoice {'{{ invoice_id }}'}"
		value={data.processName ?? ''}
		disabled={readonly}
		oninput={(e) =>
			handleProcessNameChange((e.currentTarget as HTMLInputElement).value)}
	/>
	{#if scope.length > 0}
		<InsertRefButton
			{scope}
			disabled={readonly}
			oninsert={appendToProcessName}
		/>
	{/if}
	<p class="text-sm text-muted-foreground">
		Optional. When set, each instance registers a named process (shown in the
		process list and completed when the workflow ends).
		<code>{'{{ ref }}'}</code> placeholders interpolate initial-token fields declared below.
		Leave empty to opt out.
	</p>
</div>

<PortsSection
	port={initial}
	{readonly}
	title="Initial token fields"
	emptyHint="No initial fields. Instances of this template will start with an empty token (system fields only). Add fields to require typed input at instance creation."
	onchange={handlePortChange}
/>

{#if binding && nodeId}
	<div class="space-y-1.5" data-testid="start-entrypoints">
		<div class="flex items-center justify-between">
			<span class="text-sm font-medium text-muted-foreground">Entrypoints</span>
			{#if canEditGraph}
				<Button variant="ghost" size="sm" onclick={addTrigger} data-testid="btn-add-trigger">
					<Plus class="size-3.5" />
					Add trigger
				</Button>
			{/if}
		</div>

		{#if feedingTriggers.length === 0}
			<p class="rounded-md border border-dashed border-border/50 p-2 text-sm text-muted-foreground">
				No triggers. This Start only runs when an instance is created manually
				(Run button / API). Add a trigger to fire it on a schedule, webhook,
				catalogue event, or another workflow's completion.
			</p>
		{:else}
			<ul class="space-y-1">
				{#each feedingTriggers as t (t.id)}
					<li>
						<button
							type="button"
							class="flex w-full items-center justify-between gap-2 rounded-md border border-border/60 bg-muted/20 p-2 text-left transition-colors hover:bg-muted/40"
							onclick={() => onselectnode?.(t.id)}
							data-testid="trigger-link"
						>
							<span class="flex items-center gap-2 truncate">
								<Zap class="size-3.5 shrink-0 text-node-decision" />
								<span class="truncate text-sm font-medium text-foreground">
									{t.label}
								</span>
								<span class="text-sm uppercase tracking-wide text-muted-foreground/70">
									{sourceKindLabels[t.kind] ?? t.kind}
								</span>
							</span>
							{#if !t.enabled}
								<span
									class="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-sm uppercase tracking-wide text-muted-foreground"
								>
									Disabled
								</span>
							{/if}
						</button>
					</li>
				{/each}
			</ul>
		{/if}
	</div>
{/if}
