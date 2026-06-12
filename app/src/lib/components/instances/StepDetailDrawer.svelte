<script lang="ts">
	import { Sheet, SheetContent, SheetTitle, SheetDescription, SheetClose } from '$lib/components/ui/sheet';
	import * as Tabs from '$lib/components/ui/tabs';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Dialog } from 'bits-ui';
	import X from '@lucide/svelte/icons/x';
	import Settings2 from '@lucide/svelte/icons/settings-2';
	import Workflow from '@lucide/svelte/icons/workflow';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import CopyButton from '$lib/components/ui/copy-button/CopyButton.svelte';
	import type { AllocationResponse, Channel, CatalogueEntry, InstanceChild, StepExecution, WorkflowNode } from '$lib/api/client';
	import { listCatalogueEntries } from '$lib/api/client';
	import type { NodeInterface } from '$lib/types/node-interface';
	import type { ChannelRuntime, LeaseRuntime } from '$lib/stores/instance-marking.svelte';
	import { nodeKindMeta } from './node-kind-meta';
	import InspectorShell from '$lib/components/inspector/InspectorShell.svelte';
	import { SmartValue } from './output-renderers';
	import ArtifactMediaPreview from '$lib/components/catalogue/ArtifactMediaPreview.svelte';
	import FileBox from '@lucide/svelte/icons/file-box';
	import ChannelsPanel from './ChannelsPanel.svelte';
	import StreamSinkPanel from './StreamSinkPanel.svelte';
	import StreamSourcePanel from './StreamSourcePanel.svelte';
	import StepLogs from './StepLogs.svelte';
	import Server from '@lucide/svelte/icons/server';
	import Cpu from '@lucide/svelte/icons/cpu';

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
		/** Allocation rows for this node from the `allocations` projection
		 *  (datacenter leases / token-pool grants). Rendered for Scheduled and
		 *  LeaseScope nodes; gracefully omitted when empty. */
		allocationRows?: AllocationResponse[];
		/** Per-channel live lifecycle for this node, keyed by channel name,
		 *  derived by the parent from the instance net marking. Absent → the
		 *  Channels section renders the declared channels statically (no faked
		 *  lifecycle). */
		channelRuntime?: Record<string, ChannelRuntime> | null;
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
		allocationRows = [],
		channelRuntime = null,
		open,
		onClose,
		onSelectIteration
	}: Props = $props();

	// Statically-declared channels on this node (docs/25). They live on the
	// channel-carrying arms of `WorkflowNodeData` — `automated_step` plus the
	// streaming endpoint nodes (`stream_source`/`stream_sink`); narrow on the
	// arm before reading. Surfaced in the Channels section; lifecycle (if
	// available) comes from `channelRuntime`.
	const channels = $derived<Channel[]>(
		node?.data?.type === 'automated_step' ||
			node?.data?.type === 'stream_source' ||
			node?.data?.type === 'stream_sink'
			? (node.data.channels ?? [])
			: []
	);

	// A StreamSink node re-exposes its consumed stream at a stable per-instance
	// egress URL — the drawer surfaces it (plus a live preview when renderable).
	// A sink runs no executor job, so it usually has NO step row: the dedicated
	// sheet branch below keeps the drawer non-empty for it (mirroring LeaseScope).
	const isStreamSink = $derived(node?.data?.type === 'stream_sink');

	// A StreamSource is the mirror image: external producers PUSH into its
	// stable per-instance ingress URL. It runs no executor job either, so it
	// normally has no step row — its dedicated branch shows the ingress view.
	const isStreamSource = $derived(node?.data?.type === 'stream_source');

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

	// Show the Allocation panel for Scheduled AutomatedSteps and LeaseScope
	// containers. Gracefully omitted when there are no rows (e.g. pre-run or
	// non-scheduler nodes whose allocations are empty).
	const showAllocationPanel = $derived(
		(kind === 'scheduled' || kind === 'lease_scope') && allocationRows.length > 0
	);

	// Status palette for allocation rows — mirrors the allocation status values
	// from the schema (`pending | held | released | failed | expired`).
	const allocStatusColor: Record<string, string> = {
		pending:  'bg-amber-100 text-amber-700',
		held:     'bg-green-100 text-green-700',
		released: 'bg-slate-100 text-slate-600',
		failed:   'bg-red-100 text-red-700',
		expired:  'bg-orange-100 text-orange-700'
	};
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

	// ── Step-scoped logs ──────────────────────────────────────────────────────
	// `execution_id` is the reliable key for scoping `hpi_logs` to this exact
	// step+iteration AND for addressing the step's data-plane channel bytes via
	// the datastream tap. The projection now surfaces it as a first-class
	// `step.execution_id` (hoisted off the envelope before `outputs` is unwrapped
	// to its business fields); fall back to the envelope for legacy rows.
	const stepOutputs = $derived<Record<string, unknown> | null>(
		step?.outputs && typeof step.outputs === 'object'
			? (step.outputs as Record<string, unknown>)
			: null
	);
	const stepExecutionId = $derived<string | null>(
		step?.execution_id ??
			(typeof stepOutputs?.execution_id === 'string'
				? (stepOutputs.execution_id as string)
				: null)
	);
	// Files this step registered via the SDK `log_artifact(...)`. They don't ride
	// in `step.outputs` (that carries only the business outputs), so we pull them
	// from the durable file catalogue keyed by the step's execution_id — which is
	// also where the correct `storage_path` / `mime_type` live for download +
	// inline preview.
	let stepArtifacts = $state<CatalogueEntry[]>([]);
	$effect(() => {
		const eid = stepExecutionId;
		stepArtifacts = [];
		if (!eid) return;
		let cancelled = false;
		listCatalogueEntries({ execution_id: eid, page_size: 50 })
			.then((res) => {
				if (!cancelled) stepArtifacts = res.items ?? [];
			})
			.catch(() => {
				/* best-effort: no artifacts section if the lookup fails */
			});
		return () => {
			cancelled = true;
		};
	});

	const stepLogsSummary = $derived.by<{ total: number | null; byLevel: Record<string, number> | null }>(() => {
		const d = stepOutputs?.detail;
		const logs =
			d && typeof d === 'object' ? (d as Record<string, unknown>).logs : undefined;
		if (!logs || typeof logs !== 'object') return { total: null, byLevel: null };
		const l = logs as Record<string, unknown>;
		return {
			total: typeof l.total_entries === 'number' ? l.total_entries : null,
			byLevel:
				l.count_by_level && typeof l.count_by_level === 'object'
					? (l.count_by_level as Record<string, number>)
					: null
		};
	});
	// Show the Logs section for executor-backed steps (those carry an
	// execution_id or a reported log count); control-flow nodes (Start /
	// Condition / End) have no executor logs, so we skip the section entirely
	// rather than render an always-empty one.
	const showStepLogs = $derived<boolean>(
		!!instanceId &&
			!!step?.started_at &&
			(!!stepExecutionId || (stepLogsSummary.total ?? 0) > 0)
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

	// Inputs (the parked envelopes of upstream producers) are usually the bulkier
	// of the two and most of the time not what you're looking at, so the I/O view
	// is a tab pair defaulting to Outputs. The picker resets to Outputs whenever a
	// different step is selected.
	const inputCount = $derived(inputEntries(step?.inputs).length);
	const hasOutputs = $derived(step?.outputs !== null && step?.outputs !== undefined);
	let ioTab = $state<'outputs' | 'inputs'>('outputs');
	$effect(() => {
		// Re-key on the selected step+iteration so switching steps lands on Outputs.
		void `${step?.node_id}:${step?.iteration_index}`;
		ioTab = 'outputs';
	});
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

<!-- Allocation sub-panel: one row per resource grant. Rendered for Scheduled
     AutomatedSteps and LeaseScope containers. A LeaseScope may carry multiple
     grants (one per loop iteration); a Scheduled step typically carries one.
     Fields are best-effort (nullable) — Phase-1 accounting fills them in as
     the grant progresses; absent fields are shown as em-dashes. -->
{#snippet allocationPanel(rows: AllocationResponse[])}
	<section data-testid="allocation-panel">
		<h3 class="mb-2 flex items-center gap-2 text-sm font-semibold text-foreground">
			<Cpu class="size-4 text-muted-foreground" />
			Allocation{rows.length > 1 ? `s (${rows.length})` : ''}
		</h3>
		<div class="divide-y divide-border rounded-md border border-border">
			{#each rows as row (row.id)}
				{@const statusCls = allocStatusColor[row.status] ?? 'bg-gray-100 text-gray-700'}
				<div class="px-3 py-2 text-sm">
					<!-- Row header: alloc_id + flavor + status pill + cluster back-link -->
					<div class="mb-1.5 flex flex-wrap items-center gap-1.5">
						{#if row.alloc_id}
							<span class="font-mono text-foreground break-all">{row.alloc_id}</span>
						{:else}
							<span class="font-mono text-muted-foreground">—</span>
						{/if}
						{#if row.scheduler_flavor}
							<Badge variant="outline" class="font-mono text-sm font-normal">{row.scheduler_flavor}</Badge>
						{/if}
						<Badge class="{statusCls} font-normal" variant="secondary">{row.status}</Badge>
						{#if row.cluster_resource_id}
							<a
								href="/clusters/{row.cluster_resource_id}"
								class="text-sm text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
								title="View cluster"
							>cluster &rarr;</a>
						{/if}
					</div>
					<!-- Detail grid -->
					<dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
						{#if row.node}
							<dt class="text-muted-foreground">node</dt>
							<dd class="font-mono break-all">{row.node}</dd>
						{/if}
						<dt class="text-muted-foreground">queue wait</dt>
						<dd class="font-mono">{formatDuration(row.queue_wait_ms)}</dd>
						<dt class="text-muted-foreground">runtime</dt>
						<dd class="font-mono">{formatDuration(row.duration_ms)}</dd>
						{#if row.exit_code !== null && row.exit_code !== undefined}
							<dt class="text-muted-foreground">exit code</dt>
							<dd class="font-mono">{row.exit_code}</dd>
						{/if}
						{#if row.cpu_seconds !== null && row.cpu_seconds !== undefined}
							<dt class="text-muted-foreground">CPU-hours</dt>
							<dd class="font-mono">{(row.cpu_seconds / 3600).toFixed(3)}</dd>
						{/if}
						{#if row.gpu_seconds !== null && row.gpu_seconds !== undefined}
							<dt class="text-muted-foreground">GPU-hours</dt>
							<dd class="font-mono">{(row.gpu_seconds / 3600).toFixed(3)}</dd>
						{/if}
					</dl>
					{#if row.last_error}
						<div class="mt-1.5 rounded-md border border-destructive/30 bg-destructive/5 px-2 py-1 text-sm text-destructive font-mono break-words">
							{row.last_error}
						</div>
					{/if}
				</div>
			{/each}
		</div>
	</section>
{/snippet}

<!-- Shared header affordances, reused across every drawer branch so the chrome
     stays identical whether or not a step row exists. -->
{#snippet closeButton()}
	<SheetClose>
		<Button variant="ghost" size="icon" aria-label="Close">
			<X class="size-4" />
		</Button>
	</SheetClose>
{/snippet}

{#snippet configButton()}
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

			<InspectorShell kind={kind} label={nodeLabel} nodeId={step.node_id} description={nodeDescription}>
				{#snippet status()}
					<Badge class={statusColor[step.status] ?? ''} variant="secondary">
						{step.status}
					</Badge>
					{#if step.iteration_index > 0 || iterations.length > 1}
						<Badge variant="outline">iter {step.iteration_index}</Badge>
					{/if}
					{#if step.branch_taken}
						<Badge variant="outline" class="font-mono">→ {step.branch_taken}</Badge>
					{/if}
				{/snippet}
				{#snippet actions()}
					<CopyButton
						getText={() => pretty(step)}
						title="Copy the full step execution as JSON"
					/>
					{@render configButton()}
				{/snippet}
				{#snippet close()}
					{@render closeButton()}
				{/snippet}

				{#if leaseRuntime}
					{@render leasePanel(leaseRuntime)}
				{/if}
				{#if showAllocationPanel}
					{@render allocationPanel(allocationRows)}
				{/if}
				{#if showChildren}
					<!-- Sub-workflow drill-in: each child ran as its own instance
					     (a separate engine net). Navigating to it is a fresh
					     /instances/[id] mount — a plain <a> is correct. -->
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

				{#if showStepLogs}
					<!-- First-class, step+iteration-scoped logs for the selected node.
					     Fetches `hpi_logs` narrowed to this step's execution_id (the
					     reliable per-step key) over its time window — the same lines
					     the Logs tab shows globally, but scoped to what's selected. -->
					<section data-testid="step-logs">
						<!-- Remount per step+iteration: the drawer reuses one drawer
						     instance across node selections, so a fresh key resets the
						     cached log lines for the newly-selected step. -->
						{#key `${step.node_id}:${step.iteration_index}`}
							<StepLogs
								{instanceId}
								executionId={stepExecutionId}
								startedAt={step.started_at}
								completedAt={step.completed_at}
								expectedCount={stepLogsSummary.total}
								countByLevel={stepLogsSummary.byLevel}
								defaultOpen={(stepLogsSummary.total ?? 0) > 0}
							/>
						{/key}
					</section>
				{/if}

				<section>
					<!-- Inputs (upstream parked envelopes) and Outputs share one tab
					     pair. Inputs are usually bulkier and rarely what you're after,
					     so Outputs is the default; the picker resets per selected step. -->
					<Tabs.Root bind:value={ioTab}>
						<Tabs.List class="mb-3">
							<Tabs.Trigger value="outputs">
								Outputs
								{#if !hasOutputs}
									<span class="ml-1 text-sm font-normal text-muted-foreground/70">none</span>
								{/if}
							</Tabs.Trigger>
							<Tabs.Trigger value="inputs">
								Inputs
								{#if inputCount > 0}
									<span class="ml-1 font-mono text-sm font-normal text-muted-foreground/80">{inputCount}</span>
								{:else}
									<span class="ml-1 text-sm font-normal text-muted-foreground/70">none</span>
								{/if}
							</Tabs.Trigger>
						</Tabs.List>

						<Tabs.Content value="outputs">
							{#if hasOutputs}
								<SmartValue
									value={step.outputs}
									ctx={{
										position: 'output',
										nodeKind: step.node_kind,
										instanceId,
										stepStartedAt: step.started_at ?? undefined,
										stepCompletedAt: step.completed_at ?? undefined,
										// The drawer renders its own first-class Logs section
										// above; tell the envelope not to also show its inline
										// logs block (same execution → same lines).
										suppressLogs: showStepLogs
									}}
								/>
							{:else}
								<p class="text-sm text-muted-foreground italic">This step produced no outputs.</p>
							{/if}
						</Tabs.Content>

						<Tabs.Content value="inputs">
							{#if inputCount > 0}
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
							{:else}
								<p class="text-sm text-muted-foreground italic">This step read no upstream inputs.</p>
							{/if}
						</Tabs.Content>
					</Tabs.Root>
				</section>

				{#if stepArtifacts.length > 0}
					<section>
						<h3 class="mb-2 flex items-center gap-1.5 text-sm font-semibold text-foreground">
							<FileBox class="size-4 text-muted-foreground" />
							Artifacts
							<Badge variant="secondary" class="font-mono text-sm">{stepArtifacts.length}</Badge>
						</h3>
						<div class="space-y-2">
							{#each stepArtifacts as a (a.entry_id ?? a.id ?? a.storage_path)}
								<div class="rounded-md border border-border bg-muted/20 p-3">
									<div class="mb-1.5 flex flex-wrap items-center gap-2 text-sm">
										<span class="truncate font-medium text-foreground">{a.name}</span>
										{#if a.category}
											<Badge variant="outline" class="font-mono text-sm">{a.category}</Badge>
										{/if}
									</div>
									<ArtifactMediaPreview
										storagePath={a.storage_path ?? null}
										mimeType={a.mime_type ?? null}
										filename={a.filename}
										name={a.name}
										sizeBytes={a.size_bytes ?? null}
									/>
								</div>
							{/each}
						</div>
					</section>
				{/if}

				{#if isStreamSink && node && instanceId}
					<!-- Stable egress URL + live preview for the sink's stream. Rendered
					     here too in case a sink ever carries a step row; the dedicated
					     no-step branch below is the usual path. -->
					<StreamSinkPanel {instanceId} nodeId={node.id} {channels} runtime={channelRuntime} />
				{/if}

				{#if channels.length > 0}
					<!-- Declared streaming channels (docs/25). Static list of
					     name/direction/plane/element; best-effort live "opened · N
					     elements · closed" status when the marking is available; a
					     Play/Preview affordance for OUT data channels carrying
					     audio/video/image (taps the channel-data endpoint). -->
					<ChannelsPanel
						{channels}
						runtime={channelRuntime}
						executionId={stepExecutionId}
					/>
				{/if}
			</InspectorShell>
		{:else if leaseRuntime}
			<!-- LeaseScope container: no step-execution row of its own. The drawer
			     is the cluster view — the held allocation's lifecycle + placement.
			     allocationPanel is appended below when rows are available. -->
			<SheetTitle>{nodeLabel} — {meta.label}</SheetTitle>
			<SheetDescription>Cluster lease held by this scope.</SheetDescription>

			<InspectorShell kind={kind} label={nodeLabel} nodeId={node?.id} description={nodeDescription}>
				{#snippet actions()}{@render configButton()}{/snippet}
				{#snippet close()}{@render closeButton()}{/snippet}

				{@render leasePanel(leaseRuntime)}
				{#if showAllocationPanel}
					{@render allocationPanel(allocationRows)}
				{/if}
			</InspectorShell>
		{:else if isStreamSink && node && instanceId}
			<!-- StreamSink endpoint: no step-execution row of its own (it runs no
			     executor job). The drawer is the egress view — the stable URL
			     external consumers tap, plus a live preview when renderable. -->
			<SheetTitle>{nodeLabel} — {meta.label}</SheetTitle>
			<SheetDescription>Stream egress endpoint for this node.</SheetDescription>

			<InspectorShell kind={kind} label={nodeLabel} nodeId={node.id} description={nodeDescription}>
				{#snippet actions()}{@render configButton()}{/snippet}
				{#snippet close()}{@render closeButton()}{/snippet}

				<StreamSinkPanel {instanceId} nodeId={node.id} {channels} runtime={channelRuntime} />
				{#if channels.length > 0}
					<ChannelsPanel {channels} runtime={channelRuntime} executionId={null} />
				{/if}
			</InspectorShell>
		{:else if isStreamSource && node && instanceId}
			<!-- StreamSource endpoint: no step-execution row of its own (it runs no
			     executor job — mekhan is the virtual producer). The drawer is the
			     ingress view: the stable URL(s) external producers push to. -->
			<SheetTitle>{nodeLabel} — {meta.label}</SheetTitle>
			<SheetDescription>Stream ingress endpoint for this node.</SheetDescription>

			<InspectorShell kind={kind} label={nodeLabel} nodeId={node.id} description={nodeDescription}>
				{#snippet actions()}{@render configButton()}{/snippet}
				{#snippet close()}{@render closeButton()}{/snippet}

				<StreamSourcePanel {instanceId} nodeId={node.id} {channels} runtime={channelRuntime} />
				{#if channels.length > 0}
					<ChannelsPanel {channels} runtime={channelRuntime} executionId={null} />
				{/if}
			</InspectorShell>
		{:else if showAllocationPanel}
			<!-- No step row and no lease-marking yet, but allocations are present
			     (e.g. a released LeaseScope where the marking was already cleaned up).
			     Render a minimal header + the allocation table so the data isn't lost. -->
			<SheetTitle>{nodeLabel} — {meta.label}</SheetTitle>
			<SheetDescription>Allocation detail for this node.</SheetDescription>

			<InspectorShell kind={kind} label={nodeLabel} nodeId={node?.id} description={nodeDescription}>
				{#snippet actions()}{@render configButton()}{/snippet}
				{#snippet close()}{@render closeButton()}{/snippet}

				{@render allocationPanel(allocationRows)}
			</InspectorShell>
		{:else}
			<!-- Fallback: the node has no step-execution row (it hasn't started —
			     e.g. a pending step on a running instance) and none of the
			     dedicated no-step views apply. Without this branch the sheet
			     rendered EMPTY (a blank panel) for every not-yet-run node. -->
			<SheetTitle>{nodeLabel} — {meta.label}</SheetTitle>
			<SheetDescription>This step has not started yet.</SheetDescription>

			<InspectorShell kind={kind} label={nodeLabel} nodeId={node?.id} description={nodeDescription}>
				{#snippet status()}
					<Badge variant="outline" class="text-muted-foreground">pending</Badge>
				{/snippet}
				{#snippet actions()}{@render configButton()}{/snippet}
				{#snippet close()}{@render closeButton()}{/snippet}

				<p class="text-sm text-muted-foreground">
					No runtime activity yet — this step hasn't been reached. Its configuration is
					available via the Config button above.
				</p>
				{#if channels.length > 0}
					<ChannelsPanel {channels} runtime={channelRuntime} executionId={null} />
				{/if}
			</InspectorShell>
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
