<script lang="ts">
	import { untrack } from 'svelte';
	import {
		getTemplate,
		listStepExecutions,
		listInstanceChildren,
		listAllocations,
		type AllocationResponse,
		type InstanceChild,
		type StepExecution,
		type Template,
		type WorkflowInstance,
		type WorkflowNode
	} from '$lib/api/client';
	import type { WorkflowGraph } from '$lib/api/client';
	import { parseInterfaceRegistry, type InterfaceRegistry } from '$lib/types/node-interface';
	import WorkflowCanvas from '$lib/components/editor/WorkflowCanvas.svelte';
	import StepDetailDrawer from './StepDetailDrawer.svelte';
	import { provideNodeRuntime, provideAwaitingResource } from './runtime-context';
	import { provideEdgeFeeds, deriveEdgeFeeds } from './edge-feed-context';
	import {
		createInstanceMarkingStore,
		isAwaitingResource,
		leaseRuntimeFor,
		channelRuntimeFor,
		type ChannelRuntime,
		type LeaseRuntime
	} from '$lib/stores/instance-marking.svelte';
	import { PoolContentionView } from '$lib/components/petri';
	import { groupChildrenByNode } from './subworkflow-children';
	import { tryUseInstanceContext } from './instance-context';
	import { RefreshScheduler } from './instance-graph-refresh';

	type Props = {
		instance: WorkflowInstance;
	};

	let { instance }: Props = $props();

	// The layout holds ONE instance SSE stream and bumps `structuralEventTick`
	// on each non-noise domain event. We subscribe to that tick to drive
	// event-driven projection refetches (below) instead of blind 2 s polling.
	// Nullable: if this view is ever mounted outside the /instances/[id] layout
	// (standalone embed / isolation), we degrade to plain polling.
	const instanceCtx = tryUseInstanceContext();

	let template = $state<Template | null>(null);
	let executions = $state<StepExecution[]>([]);
	let children = $state<InstanceChild[]>([]);
	let allocations = $state<AllocationResponse[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let drawerStep = $state<StepExecution | null>(null);
	let drawerNode = $state<WorkflowNode | null>(null);
	let drawerNodeInterface = $state<import('$lib/types/node-interface').NodeInterface | null>(null);
	let drawerIterations = $state<StepExecution[]>([]);
	let drawerOpen = $state(false);

	const isTerminal = $derived(
		instance.status === 'completed' ||
			instance.status === 'failed' ||
			instance.status === 'cancelled'
	);

	// `node_id → executions[]` so Loop body nodes can carry every iteration's
	// row. Ordered by iteration_index for deterministic "latest" lookups.
	const executionsByNode = $derived.by(() => {
		const map = new Map<string, StepExecution[]>();
		for (const e of executions) {
			const list = map.get(e.node_id) ?? [];
			list.push(e);
			map.set(e.node_id, list);
		}
		for (const list of map.values()) {
			list.sort((a, b) => a.iteration_index - b.iteration_index);
		}
		return map;
	});

	// Provide the lookup to every descendant node component via Svelte
	// context. `WorkflowNodeCard` (composed by every standard node) and
	// `LoopNode` read it through `useNodeRuntime` and render a status badge.
	provideNodeRuntime((nodeId: string) => executionsByNode.get(nodeId) ?? []);

	// ── Resource-pool "waiting for resource" overlay (M3) ────────────────────
	// Reads the instance net marking (same /petri/api/nets source the pool
	// view uses) and exposes the per-node predicate via context so the badge
	// can light up without prop-drilling through xyflow. The store owns NO
	// timer — its `refresh()` is folded into the existing 2 s poll below, so
	// the instance view keeps a single poll. Only created once the instance
	// actually has a deployed net (net_id present, not `created`).
	// Created once from the instance's net at mount (the view remounts per
	// instance id), so the initial-value read is intended.
	// svelte-ignore state_referenced_locally
	const marking = createInstanceMarkingStore(instance.net_id ?? '');

	// Bump on every marking refresh so the derived predicate / waiting-set
	// recompute. (`marking.count` reads `$state` internally; this tick makes
	// the dependency explicit for the `$derived` consumers below.)
	let markingTick = $state(0);

	// Per-node predicate, read by NodeRuntimeBadge through context. Reading
	// `markingTick` ties the lookup's freshness to each poll cycle.
	provideAwaitingResource((nodeId: string) => {
		void markingTick;
		return isAwaitingResource(marking, nodeId);
	});

	// The set of node ids currently awaiting a resource grant — for any
	// in-instance PoolContentionView (`waitingNodeIds` prop). Recomputed each
	// poll tick across the graph's nodes.
	const waitingNodeIds = $derived.by(() => {
		void markingTick;
		const s = new Set<string>();
		if (!graph) return s;
		for (const n of graph.nodes) {
			if (isAwaitingResource(marking, n.id)) s.add(n.id);
		}
		return s;
	});

	// `parent_node_id → child instances[]` (ordered by spawn/iteration order)
	// so the drawer can offer an "Enter sub-workflow" drill-in per SubWorkflow
	// node. A SubWorkflow inside a Loop/Map spawns one child per iteration.
	const childrenByNode = $derived(groupChildrenByNode(children));

	// Children for the node the drawer is currently showing.
	const drawerChildren = $derived(
		drawerNode ? (childrenByNode.get(drawerNode.id) ?? []) : []
	);

	// `node_id → AllocationResponse[]` — keyed by the grant's `node_id` field
	// (the LeaseScope container id or the Scheduled AutomatedStep id that held
	// the grant). Used by the drawer to surface per-node allocation detail.
	const allocationsByNode = $derived.by(() => {
		const map = new Map<string, AllocationResponse[]>();
		for (const a of allocations) {
			if (!a.node_id) continue;
			const list = map.get(a.node_id) ?? [];
			list.push(a);
			map.set(a.node_id, list);
		}
		return map;
	});

	// Allocations for the node currently shown in the drawer.
	const drawerAllocations = $derived(
		drawerNode ? (allocationsByNode.get(drawerNode.id) ?? []) : []
	);

	// Prefer the per-run snapshot captured on the instance. A DRAFT dev-run
	// compiles from the live Y.Doc, so `template.graph` is the stale pre-publish
	// topology — rendering it would show the canvas as it was BEFORE the user's
	// edits (the "my changes aren't reflected in the run" bug). `graph_snapshot`
	// is the graph that actually ran; NULL for live/test_run, where the
	// immutable published `template.graph` is the correct source.
	const graph = $derived<WorkflowGraph | null>(
		instance.graph_snapshot
			? (instance.graph_snapshot as WorkflowGraph)
			: template?.graph
				? (template.graph as WorkflowGraph)
				: null
	);

	// `node_id → WorkflowNode` lookup so the drawer can show the node's
	// label/description and its raw config payload.
	const nodesById = $derived.by(() => {
		const map = new Map<string, WorkflowNode>();
		if (!graph) return map;
		for (const n of graph.nodes) map.set(n.id, n);
		return map;
	});

	// Compiler-derived per-node interface (entry/data_port/owned_*/borrowed_paths).
	// `template.interface_json` is typed as `unknown` over the wire; coerce
	// once and look up by node id when opening the drawer.
	const interfaceRegistry = $derived<InterfaceRegistry>(
		parseInterfaceRegistry(instance.interface_snapshot ?? template?.interface_json)
	);

	async function loadTemplate() {
		try {
			template = await getTemplate(instance.template_id);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load template';
		}
	}

	async function refreshExecutions() {
		try {
			executions = await listStepExecutions(instance.id);
		} catch (e) {
			// Keep the canvas visible even if the projection fetch transiently
			// fails — the badge just won't render.
			error = e instanceof Error ? e.message : String(e);
		}
	}

	// Pull new instance-net events and re-fold the marking. Folded into the
	// SAME poll cycle as `refreshExecutions` — no separate timer. Skipped when
	// the instance has no deployed net yet (`created`).
	async function refreshMarking() {
		if (!instance.net_id) return;
		await marking.refresh();
		markingTick++;
	}

	async function refreshChildren() {
		try {
			children = await listInstanceChildren(instance.id);
		} catch {
			// Non-fatal: drill-in just won't appear this tick.
		}
	}

	async function refreshAllocations() {
		try {
			allocations = await listAllocations(instance.id);
		} catch {
			// Non-fatal: allocation detail just won't appear this tick.
		}
	}

	// One poll cycle — executions (drives the per-node status badges), marking
	// (channel/lease runtime + on-edge feeds), children, allocations. Shared by
	// the live 2 s interval and the terminal catch-up below.
	function refreshAll() {
		void refreshExecutions();
		void refreshMarking();
		void refreshChildren();
		void refreshAllocations();
	}

	// Depend on the id VALUE, not the `instance` prop object. The parent re-fetches
	// the instance every poll and passes a NEW object with the same id; a bare
	// `instance.id` read makes this effect depend on the `instance` signal, so it
	// re-ran every poll — re-running `loadTemplate()` (new `template` → new `graph`
	// identity → WorkflowCanvas rebuilds its edge array → xyflow recreates every
	// edge component) and firing the `marking.destroy()` cleanup. A value-compared
	// `$derived` only propagates when the id actually changes (real navigation).
	const instanceId = $derived(instance.id);
	$effect(() => {
		void instanceId; // sole tracked dep (value-compared → fires only on real nav)
		// `untrack` the body: the init functions below synchronously read
		// `instance.template_id` / `.id` / `.net_id` (before their first await), which
		// would otherwise make this effect depend on the whole `instance` prop object.
		// The parent re-passes a new `instance` object every poll (status updates), so
		// without untrack the effect re-ran each poll — reloading the template (new
		// `graph` identity → xyflow rebuilds every edge → on-edge media flickered) and
		// firing `marking.destroy()`. Untracked, it re-runs only when the id changes.
		untrack(() => {
			loading = true;
			error = null;
			// Drilling parent→child is a param-only navigation within the same
			// /instances/[id] route, so this component is reused (not remounted)
			// and the drawer state survives. Reset it here so a leftover drawer
			// from the parent run (pointing at its SubWorkflow step) doesn't linger
			// over the child's graph.
			drawerOpen = false;
			drawerStep = null;
			drawerNode = null;
			drawerIterations = [];
			void (async () => {
				// `marking.refresh()` does the one-time topology+log load on first
				// call (when topology is still null), then incremental pulls.
				await Promise.all([
					loadTemplate(),
					refreshExecutions(),
					refreshMarking(),
					refreshChildren(),
					refreshAllocations()
				]);
				loading = false;
			})();
		});
		return () => marking.destroy();
	});

	// ── Event-driven refresh (replaces the old blind 2 s poll) ───────────────
	// The instance projection tables (step executions, marking, children,
	// allocations) are written by a SEPARATE causality consumer and LAG the raw
	// domain events. So a structural SSE event means "a projection update is
	// imminent" — we SCHEDULE a coalesced refetch, never assume the row is
	// already there. The scheduler debounces a burst into one refetch, then
	// fires ONE short follow-up to pick up the just-arrived event's lagging row.
	//
	// Timings: 300 ms debounce coalesces an event burst; +1000 ms follow-up
	// rides out the projection-consumer lag. A SLOW 12 s safety-net poll
	// guarantees a dropped/missed SSE event can't permanently stale the view.
	const REFRESH_DEBOUNCE_MS = 300;
	const REFRESH_FOLLOWUP_MS = 1000;
	const SAFETY_NET_POLL_MS = 6000;
	const FALLBACK_POLL_MS = 2000;

	// The coalescing scheduler that turns SSE structural events into debounced
	// projection refetches. Owned by an effect (created/disposed with the
	// component lifetime), poked by the tick effect. Only used when the layout
	// context is present; null otherwise (no-context → fall back to polling).
	let eventScheduler: RefreshScheduler | null = null;
	$effect(() => {
		if (!instanceCtx) return; // no layout context → fall back to polling below
		const scheduler = new RefreshScheduler(refreshAll, {
			debounceMs: REFRESH_DEBOUNCE_MS,
			followUpMs: REFRESH_FOLLOWUP_MS
		});
		eventScheduler = scheduler;
		return () => {
			scheduler.dispose();
			eventScheduler = null;
		};
	});

	// React to the layout's SSE tick: a fresh non-noise structural event bumps
	// `structuralEventTick`, which is the sole tracked dependency here, so this
	// effect re-runs and schedules a coalesced refetch. Skip the initial value —
	// the mount effect already did the first load. `untrack` the scheduler poke
	// so reading it doesn't add a dependency (and a scheduler re-create from the
	// effect above doesn't re-run this).
	let lastSeenTick = -1;
	$effect(() => {
		if (!instanceCtx) return;
		const tick = instanceCtx.structuralEventTick;
		if (lastSeenTick === -1) {
			lastSeenTick = tick; // baseline; the mount load covers this
			return;
		}
		if (tick === lastSeenTick) return;
		lastSeenTick = tick;
		// The terminal NetCompleted/NetCancelled events are themselves structural,
		// so they bump the tick. Don't schedule a post-terminal refetch here — the
		// terminal effect below owns the final reconcile (immediate + settle). This
		// keeps "no live timer runs past terminal" literally true.
		if (isTerminal) return;
		untrack(() => eventScheduler?.notify());
	});

	$effect(() => {
		if (isTerminal) {
			// The instance just reached a terminal status. Any node that flipped to
			// completed/failed in the closing window (or whose projection row is
			// still folding when the row went terminal) would keep rendering a
			// stale "running" badge until a manual refresh. Streaming steps are the
			// usual victims — they close their channel, and complete, last. Catch
			// up once immediately, then once more after a short delay because the
			// instance row can go terminal a beat before the step-execution
			// projection finishes folding the final node completions. No live timer
			// runs past terminal — both the event scheduler (idle once events stop)
			// and the safety-net / fallback polls below stop themselves on terminal.
			refreshAll();
			const settle = setTimeout(refreshAll, 1500);
			return () => clearTimeout(settle);
		}
		// SLOW safety-net poll (event-driven path) / FAST fallback poll (no
		// layout context). The safety net catches a dropped SSE event so the view
		// can't permanently stale; the fallback is the old behavior when no
		// context is available to drive events.
		const period = instanceCtx ? SAFETY_NET_POLL_MS : FALLBACK_POLL_MS;
		const t = setInterval(refreshAll, period);
		return () => clearInterval(t);
	});

	// The capacity-bound AutomatedSteps in this graph (deployment `Executor { capacity }`).
	// Gating on NODE KIND — not on `p_<id>_pending` — is the key fix: a LeaseScope
	// / Scheduled step ALSO emits `p_<id>_pending` (via the shared lease bridge),
	// so the old place-based gate lit the capacity widget for cluster runs. The
	// overlay is a shared-capacity dashboard and belongs ONLY to genuine
	// seeded/presence capacity steps; cluster leases are surfaced in the drawer instead.
	const tokenPoolNodes = $derived.by(() => {
		if (!graph) return [];
		return graph.nodes.filter((n) => {
			const dm = (n.data as { deploymentModel?: { mode?: string; capacity?: unknown } } | undefined)
				?.deploymentModel;
			return dm?.mode === 'executor' && !!dm.capacity;
		});
	});
	const hasPooledNodes = $derived(tokenPoolNodes.length > 0);

	// The backing pool-net id (`pool-<resource_id>`), read from the deployed
	// instance topology's bridge_out target on the first token-pool node's
	// `claim_out` place — the alias→resource-id resolution already happened at
	// publish, so we read the resolved net id rather than re-resolving client-side
	// (and never fall back to the wrong hardcoded `resource-pool-net`).
	const poolNetId = $derived.by(() => {
		void markingTick;
		for (const n of tokenPoolNodes) {
			const target = marking.bridgeTarget(`p_${n.id}_claim_out`);
			if (target) return target;
		}
		return null;
	});

	// Lease runtime for the node the drawer currently shows (only LeaseScope
	// holds a lease). Re-derives each poll tick so the drawer's lease lifecycle +
	// placement detail stay live.
	const drawerLease = $derived.by<LeaseRuntime | null>(() => {
		void markingTick;
		if (!drawerNode || drawerNode.type !== 'lease_scope') return null;
		return leaseRuntimeFor(marking, drawerNode.id);
	});

	// Per-channel live lifecycle for the node the drawer currently shows, keyed
	// by channel name. Re-derives each poll tick so the Channels section's
	// "opened · N elements · closed" status stays live. Null when the node has
	// no declared channels (the Channels section then degrades to static).
	const drawerChannelRuntime = $derived.by<Record<string, ChannelRuntime> | null>(() => {
		void markingTick;
		// `channels` lives on the channel-carrying arms of WorkflowNodeData:
		// automated_step plus the streaming endpoint nodes (stream_source/sink).
		const d = drawerNode?.data;
		const decl =
			d?.type === 'automated_step' || d?.type === 'stream_source' || d?.type === 'stream_sink'
				? (d.channels ?? [])
				: [];
		if (decl.length === 0) return null;
		const out: Record<string, ChannelRuntime> = {};
		for (const ch of decl) out[ch.name] = channelRuntimeFor(marking, drawerNode!.id, ch.name);
		return out;
	});

	// ── On-edge live media feeds (instance/run view only) ────────────────────
	// Resolve each data-binary, live-renderable channel edge to an `EdgeFeed`
	// (source channel + latest execution_id + render plan + per-poll runtime).
	// Reading `markingTick` AND `executions` ties freshness to the existing 2 s
	// poll, so widgets stay live WITHOUT mutating the `graph` prop (which would
	// force xyflow to re-sync its edge set — see WorkflowCanvas). The context
	// getter closes over this reactive map; DeletableEdge looks itself up by id.
	const edgeFeeds = $derived.by(() => {
		void markingTick;
		void executions;
		// `isTerminal` is stamped on each feed so the widget freezes its end-state
		// (last frame held, tap + cap slot released) once the run finishes even if
		// it never observed an explicit channel `close` token. `instanceId` lets
		// stream_source producers derive their deterministic execution id
		// (`st-<instance>-<node>` — an ingress endpoint has no step execution).
		return deriveEdgeFeeds(graph, nodesById, executionsByNode, marking, isTerminal, instanceId);
	});
	provideEdgeFeeds((edgeId: string) => edgeFeeds.get(edgeId) ?? null);

	function openDrawerFor(nodeId: string) {
		const list = executionsByNode.get(nodeId) ?? [];
		const node = nodesById.get(nodeId) ?? null;
		drawerNode = node;
		drawerNodeInterface = interfaceRegistry[nodeId] ?? null;
		drawerIterations = list;
		if (list.length === 0) {
			// Step hasn't fired yet — still open the drawer so the user gets
			// the node metadata + a "View config" button, just no runtime data.
			drawerStep = null;
			drawerOpen = !!node;
			return;
		}
		drawerStep = list[list.length - 1];
		drawerOpen = true;
	}

	function selectIteration(iterationIndex: number) {
		const list = drawerIterations;
		const found = list.find((e) => e.iteration_index === iterationIndex);
		if (found) drawerStep = found;
	}

	function closeDrawer() {
		drawerOpen = false;
	}
</script>

<div class="relative h-full w-full">
	{#if loading && !graph}
		<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
			Loading workflow…
		</div>
	{:else if error && !graph}
		<div class="m-6 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{:else if graph}
		<!-- onNodeClick / onPaneClick (rather than onselect) drives the
		     drawer: those fire only on real user pointer events, so the
		     drawer never reopens on its own when xyflow re-emits selection
		     after a `store.nodes` reassignment from polled runtime data. -->
		<WorkflowCanvas
			{graph}
			readonly
			onNodeClick={openDrawerFor}
			onPaneClick={closeDrawer}
		/>
		<!-- In-context capacity-contention dashboard: ONLY for workflows with a genuine
		     seeded/presence capacity step (not cluster/lease runs). Pointed at the resolved
		     backing net id (`pool-<resource_id>`) read from the instance topology.
		     `waitingNodeIds` is the predicate set from this instance's marking. -->
		{#if hasPooledNodes && poolNetId}
			<div class="pointer-events-auto absolute right-3 top-3 z-10 w-72 max-w-[calc(100%-1.5rem)]">
				<PoolContentionView compact netId={poolNetId} {waitingNodeIds} />
			</div>
		{/if}
	{:else}
		<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
			Template not available.
		</div>
	{/if}
</div>

<StepDetailDrawer
	step={drawerStep}
	node={drawerNode}
	nodeInterface={drawerNodeInterface}
	iterations={drawerIterations}
	instanceId={instance.id}
	childInstances={drawerChildren}
	leaseRuntime={drawerLease}
	allocationRows={drawerAllocations}
	channelRuntime={drawerChannelRuntime}
	open={drawerOpen}
	onClose={closeDrawer}
	onSelectIteration={selectIteration}
/>
