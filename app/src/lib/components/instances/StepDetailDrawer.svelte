<script lang="ts">
	import { Sheet, SheetContent, SheetTitle, SheetDescription, SheetClose } from '$lib/components/ui/sheet';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Dialog } from 'bits-ui';
	import X from '@lucide/svelte/icons/x';
	import Settings2 from '@lucide/svelte/icons/settings-2';
	import Workflow from '@lucide/svelte/icons/workflow';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import CopyButton from '$lib/components/ui/copy-button/CopyButton.svelte';
	import type { InstanceChild, StepExecution, WorkflowNode } from '$lib/api/client';
	import type { NodeInterface } from '$lib/types/node-interface';
	import type { LeaseRuntime } from '$lib/stores/instance-marking.svelte';
	import { nodeKindMeta } from './node-kind-meta';
	import { SmartValue } from './output-renderers';
	import Server from '@lucide/svelte/icons/server';

	type Props = {
		step: StepExecution | null;
		/** The node from the template graph this step instantiates. Optional —
		 *  the drawer degrades gracefully when not supplied (e.g. older callers). */
		node?: WorkflowNode | null;
		/** The compiler-derived interface for this node. Carries the
		 *  `borrowed_paths` map (`producer_node_id → [attr, …]`) the drawer
		 *  uses to surface "what fields this step actually read" from each
		 *  upstream envelope. */
		nodeInterface?: NodeInterface | null;
		/** Every iteration of this node in the current instance (oldest → newest).
		 *  Drives the iteration picker for Loop bodies. When omitted or
		 *  single-element, the picker is hidden. */
		iterations?: StepExecution[];
		/** Owning workflow instance id. Forwarded into the renderer context so
		 *  envelope renderers can resolve instance-scoped backend resources
		 *  (e.g. AutomatedStepEnvelope's log lookup). */
		instanceId?: string;
		/** Sub-workflow child instances this node spawned (already filtered to
		 *  this node, ordered by spawn/iteration order). Drives the "Enter
		 *  sub-workflow" drill-in. Empty / absent for non-SubWorkflow nodes or
		 *  before the child has been registered. */
		childInstances?: InstanceChild[];
		/** Cluster-lease runtime for a LeaseScope node, derived from the instance
		 *  net marking by the parent. When present the drawer renders a Lease
		 *  section (lifecycle + typed placement detail). A LeaseScope container has
		 *  no step row, so this is what makes its drawer non-empty. */
		leaseRuntime?: LeaseRuntime | null;
		open: boolean;
		onClose: () => void;
		/** When the user picks a different iteration in the drawer, the parent
		 *  swaps `step` for the chosen row. */
		onSelectIteration?: (iterationIndex: number) => void;
	};

	let {
		step,
		node = null,
		nodeInterface = null,
		iterations = [],
		instanceId,
		childInstances = [],
		leaseRuntime = null,
		open,
		onClose,
		onSelectIteration
	}: Props = $props();

	// Cluster-lease lifecycle palette + copy.
	const leaseTone: Record<string, { bg: string; text: string; label: string }> = {
		idle:     { bg: 'bg-gray-100',   text: 'text-gray-600',   label: 'not yet claimed' },
		claiming: { bg: 'bg-amber-100',  text: 'text-amber-700',  label: 'claiming' },
		held:     { bg: 'bg-green-100',  text: 'text-green-700',  label: 'held' },
		released: { bg: 'bg-slate-100',  text: 'text-slate-600',  label: 'released' },
		failed:   { bg: 'bg-red-100',    text: 'text-red-700',    label: 'allocation died' }
	};

	function borrowedAttrsFor(producerNodeId: string): string[] {
		return nodeInterface?.borrowed_paths?.[producerNodeId] ?? [];
	}

	const statusColor: Record<string, string> = {
		pending: 'bg-gray-100 text-gray-700',
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700',
		skipped: 'bg-slate-100 text-slate-500'
	};

	// The node kind from the template's WorkflowNode (`type` is the AIR-side
	// kind, also snake_case) or from the step row (snake_case from the
	// projection). Prefer the step's kind since that's what the projection
	// actually saw; fall back to the template node when the step is unavailable.
	const kind = $derived(step?.node_kind ?? node?.type ?? 'unknown');
	const meta = $derived(nodeKindMeta(kind));
	const Icon = $derived(meta.icon);

	// Drill-in is offered only for SubWorkflow nodes that have spawned at least
	// one child run. A SubWorkflow inside a Loop/Map yields one child per
	// iteration (multiple rows), so the section lists every run.
	const showChildren = $derived(kind === 'sub_workflow' && childInstances.length > 0);
	const childStatusColor: Record<string, string> = {
		created: 'bg-gray-100 text-gray-700',
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700',
		cancelled: 'bg-slate-100 text-slate-500'
	};

	const nodeLabel = $derived<string>(
		(node?.data?.label ?? '') || step?.node_id || 'Step'
	);
	const nodeDescription = $derived<string | null>(
		node?.data?.description ?? null
	);

	let configOpen = $state(false);

	function formatDuration(ms: number | null | undefined): string {
		if (ms === null || ms === undefined) return '—';
		if (ms < 1000) return `${ms}ms`;
		if (ms < 60_000) return `${(ms / 1000).toFixed(2)}s`;
		const mins = Math.floor(ms / 60_000);
		const secs = Math.floor((ms % 60_000) / 1000);
		return `${mins}m ${secs}s`;
	}

	function pretty(json: unknown): string {
		try {
			return JSON.stringify(json, null, 2);
		} catch {
			return String(json);
		}
	}

	function inputEntries(inputs: unknown): Array<[string, unknown]> {
		if (!inputs || typeof inputs !== 'object') return [];
		return Object.entries(inputs as Record<string, unknown>);
	}
</script>

<!-- Cluster-lease section: lifecycle + typed placement detail, read from the
     instance net marking. Rendered for a LeaseScope (no step row) and appended
     to any step that also carries a lease. -->
{#snippet leasePanel(lr: LeaseRuntime)}
	{@const tone = leaseTone[lr.state] ?? leaseTone.idle}
	<section data-testid="lease-panel">
		<h3 class="mb-2 flex items-center gap-2 text-sm font-semibold text-foreground">
			<Server class="size-4 text-muted-foreground" />
			Lease
			{#if lr.flavor}
				<Badge variant="outline" class="font-mono text-sm font-normal">{lr.flavor}</Badge>
			{/if}
			<Badge class="{tone.bg} {tone.text} font-normal" variant="secondary">{tone.label}</Badge>
		</h3>
		{#if lr.state === 'failed'}
			<div class="mb-2 rounded-md border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700">
				The held allocation died mid-lease — the scope failed fast rather than
				running further work on a dead allocation.
			</div>
		{:else if lr.state === 'claiming'}
			<div class="mb-2 rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-700">
				Claim sent to the scheduler; waiting for the allocation to be granted.
			</div>
		{/if}
		{#if lr.allocId || lr.node || lr.executorNamespace || lr.expiry || Object.keys(lr.schedulerDetail).length > 0}
			<dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
				{#if lr.allocId}
					<dt class="text-muted-foreground">alloc id</dt>
					<dd class="font-mono break-all">{lr.allocId}</dd>
				{/if}
				{#if lr.node}
					<dt class="text-muted-foreground">node</dt>
					<dd class="font-mono break-all">{lr.node}</dd>
				{/if}
				{#if lr.executorNamespace}
					<dt class="text-muted-foreground">namespace</dt>
					<dd class="font-mono break-all">{lr.executorNamespace}</dd>
				{/if}
				{#if lr.expiry}
					<dt class="text-muted-foreground">expiry</dt>
					<dd class="font-mono break-all">{lr.expiry}</dd>
				{/if}
				{#each Object.entries(lr.schedulerDetail) as [k, v] (k)}
					<dt class="text-muted-foreground">{k}</dt>
					<dd class="font-mono break-all">{v}</dd>
				{/each}
			</dl>
		{:else}
			<p class="text-sm text-muted-foreground">No allocation detail yet.</p>
		{/if}
	</section>
{/snippet}

<Sheet.Root bind:open onOpenChange={(v: boolean) => { if (!v) onClose(); }}>
	<SheetContent class="w-full sm:max-w-xl">
		{#if step}
			<!-- SheetTitle/SheetDescription are sr-only by design (a11y).
			     The visible header below mirrors them. -->
			<SheetTitle>{nodeLabel} — {meta.label} ({step.status})</SheetTitle>
			<SheetDescription>
				Runtime detail for step {step.node_id}{step.iteration_index > 0 ? `, iteration ${step.iteration_index}` : ''}.
			</SheetDescription>

			<header class="flex items-start gap-3 border-b border-border px-5 py-4">
				<!-- Mirror the canvas card: kind-coloured icon chip + label. -->
				<div class="flex size-9 shrink-0 items-center justify-center rounded-md {meta.chipClass}">
					<Icon class="size-5 {meta.iconClass}" />
				</div>

				<div class="min-w-0 flex-1">
					<h2 class="text-base font-semibold text-foreground truncate">
						{nodeLabel}
					</h2>
					<div class="mt-1 flex flex-wrap items-center gap-2 text-sm">
						<Badge variant="outline" class="font-mono">{meta.label}</Badge>
						<Badge class={statusColor[step.status] ?? ''} variant="secondary">
							{step.status}
						</Badge>
						{#if step.iteration_index > 0 || iterations.length > 1}
							<Badge variant="outline">iter {step.iteration_index}</Badge>
						{/if}
						{#if step.branch_taken}
							<Badge variant="outline" class="font-mono">→ {step.branch_taken}</Badge>
						{/if}
					</div>
					<div class="mt-1 font-mono text-sm text-muted-foreground/80 truncate" title={step.node_id}>
						id: {step.node_id}
					</div>
					{#if nodeDescription}
						<p class="mt-1 text-sm text-muted-foreground line-clamp-2">{nodeDescription}</p>
					{/if}
				</div>

				<div class="flex shrink-0 items-center gap-1">
					<CopyButton
						getText={() => pretty(step)}
						title="Copy the full step execution as JSON"
					/>
					{#if node}
						<Button
							variant="ghost"
							size="sm"
							onclick={() => (configOpen = true)}
							title="View the node's saved configuration"
						>
							<Settings2 class="size-4" />
							<span class="ml-1.5 hidden sm:inline">Config</span>
						</Button>
					{/if}
					<SheetClose>
						<Button variant="ghost" size="icon" aria-label="Close">
							<X class="size-4" />
						</Button>
					</SheetClose>
				</div>
			</header>

			<div class="flex-1 overflow-y-auto px-5 py-4 space-y-5">
				{#if leaseRuntime}
					{@render leasePanel(leaseRuntime)}
				{/if}
				{#if showChildren}
					<!-- Sub-workflow drill-in: each child ran as its own instance
					     (a separate engine net). Navigating to it is a fresh
					     /instances/[id] mount — a plain <a> is correct (no
					     data-sveltekit-reload; that's only for the Yjs editor). -->
					<section>
						<h3 class="mb-2 flex items-center gap-1.5 text-sm font-semibold text-foreground">
							<Workflow class="size-4 text-muted-foreground" />
							Sub-workflow {childInstances.length > 1 ? `runs (${childInstances.length})` : 'run'}
						</h3>
						<div class="space-y-1.5">
							{#each childInstances as child, i (child.id)}
								<a
									href={`/instances/${child.id}/workflow`}
									data-testid="enter-subworkflow"
									class="group flex items-center gap-2 rounded-md border border-border px-3 py-2 text-sm transition-colors hover:border-primary hover:bg-accent"
								>
									<span class="min-w-0 flex-1 truncate">
										<span class="font-medium text-foreground">{child.template_name}</span>
										{#if childInstances.length > 1}
											<span class="ml-1 font-mono text-muted-foreground">· run {i + 1}</span>
										{/if}
									</span>
									<Badge class={childStatusColor[child.status] ?? ''} variant="secondary">
										{child.status}
									</Badge>
									<ArrowRight class="size-4 text-muted-foreground transition-transform group-hover:translate-x-0.5 group-hover:text-foreground" />
								</a>
							{/each}
						</div>
					</section>
				{/if}

				{#if iterations.length > 1 && onSelectIteration}
					<section>
						<h3 class="text-sm font-semibold text-foreground mb-2">Iterations</h3>
						<div class="flex flex-wrap gap-1">
							{#each iterations as it (it.iteration_index)}
								<button
									class="rounded-md border px-2 py-1 text-sm font-mono transition-colors
										{step.iteration_index === it.iteration_index
											? 'border-primary bg-primary/10 text-foreground'
											: 'border-border text-muted-foreground hover:bg-accent hover:text-foreground'}"
									onclick={() => onSelectIteration(it.iteration_index)}
								>
									#{it.iteration_index}
								</button>
							{/each}
						</div>
					</section>
				{/if}

				<section class="grid grid-cols-3 gap-3 text-sm">
					<div>
						<div class="text-muted-foreground">Started</div>
						<div class="font-medium">
							{step.started_at ? new Date(step.started_at).toLocaleTimeString() : '—'}
						</div>
					</div>
					<div>
						<div class="text-muted-foreground">Completed</div>
						<div class="font-medium">
							{step.completed_at ? new Date(step.completed_at).toLocaleTimeString() : '—'}
						</div>
					</div>
					<div>
						<div class="text-muted-foreground">Duration</div>
						<div class="font-medium">{formatDuration(step.duration_ms)}</div>
					</div>
				</section>

				{#if step.error}
					<section>
						<div class="mb-2 flex items-center gap-2">
							<h3 class="text-sm font-semibold text-destructive">Error</h3>
							<CopyButton text={pretty(step.error)} title="Copy error" class="text-destructive/70 hover:text-destructive" />
						</div>
						<pre class="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm font-mono whitespace-pre-wrap break-words text-destructive">{pretty(step.error)}</pre>
					</section>
				{/if}

				<section>
					<h3 class="text-sm font-semibold text-foreground mb-2">
						Inputs
						{#if inputEntries(step.inputs).length === 0}
							<span class="ml-1 text-sm font-normal text-muted-foreground">— none</span>
						{/if}
					</h3>
					{#if inputEntries(step.inputs).length > 0}
						<div class="space-y-3">
							{#each inputEntries(step.inputs) as [producerNode, envelope] (producerNode)}
								{@const reads = borrowedAttrsFor(producerNode)}
								<div>
									<div class="mb-1 flex flex-wrap items-center gap-1.5 text-sm">
										<span class="font-mono text-muted-foreground">from</span>
										<span class="font-mono text-foreground">{producerNode}</span>
										{#if reads.length > 0}
											<!-- Compiler-derived borrow surface: the field
											     attrs this step's author actually referenced
											     off `<producerNode>`. Narrows the full
											     envelope below to "what was read". -->
											<span class="text-muted-foreground">·</span>
											<span class="text-muted-foreground">reads</span>
											{#each reads as attr (attr)}
												<Badge variant="outline" class="font-mono text-sm font-normal">
													{producerNode}.{attr}
												</Badge>
											{/each}
										{/if}
									</div>
									<!-- nodeKind is left undefined: `producerNode` is a slug,
									     not a kind. The registry's shape predicates are specific
									     enough to dispatch on shape alone. -->
									<SmartValue
										value={envelope}
										ctx={{
											position: 'input',
											instanceId,
											stepStartedAt: step.started_at ?? undefined,
											stepCompletedAt: step.completed_at ?? undefined
										}}
									/>
								</div>
							{/each}
						</div>
					{/if}
				</section>

				<section>
					<h3 class="text-sm font-semibold text-foreground mb-2">
						Outputs
						{#if step.outputs === null || step.outputs === undefined}
							<span class="ml-1 text-sm font-normal text-muted-foreground">— none</span>
						{/if}
					</h3>
					{#if step.outputs !== null && step.outputs !== undefined}
						<SmartValue
							value={step.outputs}
							ctx={{
								position: 'output',
								nodeKind: step.node_kind,
								instanceId,
								stepStartedAt: step.started_at ?? undefined,
								stepCompletedAt: step.completed_at ?? undefined
							}}
						/>
					{/if}
				</section>
			</div>
		{:else if leaseRuntime}
			<!-- LeaseScope container: no step-execution row of its own. The drawer
			     is the cluster view — the held allocation's lifecycle + placement. -->
			<SheetTitle>{nodeLabel} — {meta.label}</SheetTitle>
			<SheetDescription>Cluster lease held by this scope.</SheetDescription>

			<header class="flex items-start gap-3 border-b border-border px-5 py-4">
				<div class="flex size-9 shrink-0 items-center justify-center rounded-md {meta.chipClass}">
					<Icon class="size-5 {meta.iconClass}" />
				</div>
				<div class="min-w-0 flex-1">
					<h2 class="text-base font-semibold text-foreground truncate">{nodeLabel}</h2>
					<div class="mt-1 flex flex-wrap items-center gap-2 text-sm">
						<Badge variant="outline" class="font-mono">{meta.label}</Badge>
					</div>
					{#if node}
						<div class="mt-1 font-mono text-sm text-muted-foreground/80 truncate" title={node.id}>
							id: {node.id}
						</div>
					{/if}
					{#if nodeDescription}
						<p class="mt-1 text-sm text-muted-foreground line-clamp-2">{nodeDescription}</p>
					{/if}
				</div>
				<div class="flex shrink-0 items-center gap-1">
					{#if node}
						<Button variant="ghost" size="sm" onclick={() => (configOpen = true)} title="View the node's saved configuration">
							<Settings2 class="size-4" />
							<span class="ml-1.5 hidden sm:inline">Config</span>
						</Button>
					{/if}
					<SheetClose>
						<Button variant="ghost" size="icon" aria-label="Close">
							<X class="size-4" />
						</Button>
					</SheetClose>
				</div>
			</header>

			<div class="flex-1 overflow-y-auto px-5 py-4 space-y-5">
				{@render leasePanel(leaseRuntime)}
			</div>
		{/if}
	</SheetContent>
</Sheet.Root>

<!-- Config inspector: layered above the sheet so the runtime view stays
     visible underneath. Read-only — this is a debugging/audit lens, not an
     editor; the template editor is the place to change node config. -->
<Dialog.Root bind:open={configOpen}>
	<Dialog.Portal>
		<Dialog.Overlay class="fixed inset-0 z-[60] bg-black/40 data-[state=open]:animate-in data-[state=open]:fade-in" />
		<Dialog.Content
			class="fixed left-1/2 top-1/2 z-[70] flex max-h-[85vh] w-[min(90vw,720px)] -translate-x-1/2 -translate-y-1/2 flex-col rounded-lg border border-border bg-card shadow-xl data-[state=open]:animate-in data-[state=open]:fade-in data-[state=open]:zoom-in-95"
		>
			<header class="flex items-start gap-3 border-b border-border px-5 py-3">
				<div class="flex size-7 shrink-0 items-center justify-center rounded-md {meta.chipClass}">
					<Icon class="size-4 {meta.iconClass}" />
				</div>
				<div class="min-w-0 flex-1">
					<Dialog.Title class="text-base font-semibold text-foreground">
						{nodeLabel} — configuration
					</Dialog.Title>
					<Dialog.Description class="text-sm text-muted-foreground">
						Read-only view of the node as published with the template.
					</Dialog.Description>
				</div>
				{#if node}
					<CopyButton getText={() => pretty(node)} title="Copy node configuration as JSON" />
				{/if}
				<Dialog.Close>
					<Button variant="ghost" size="icon" aria-label="Close">
						<X class="size-4" />
					</Button>
				</Dialog.Close>
			</header>

			<div class="flex-1 overflow-y-auto px-5 py-4">
				{#if node}
					<pre class="rounded-md border border-border bg-muted/30 p-3 text-sm font-mono whitespace-pre-wrap break-words">{pretty(node)}</pre>
				{:else}
					<p class="text-sm text-muted-foreground">Configuration not available for this node.</p>
				{/if}
			</div>
		</Dialog.Content>
	</Dialog.Portal>
</Dialog.Root>
