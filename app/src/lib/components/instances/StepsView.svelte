<script lang="ts">
	import {
		getTemplate,
		listStepExecutions,
		type StepExecution,
		type Template,
		type WorkflowGraph,
		type WorkflowInstance,
		type WorkflowNode
	} from '$lib/api/client';
	import { StatusBadge } from '$lib/components/status';
	import {
		parseInterfaceRegistry,
		type InterfaceRegistry,
		type NodeInterface
	} from '$lib/types/node-interface';
	import StepDetailDrawer from './StepDetailDrawer.svelte';

	type Props = {
		instance: WorkflowInstance;
	};

	let { instance }: Props = $props();

	let steps = $state<StepExecution[]>([]);
	let template = $state<Template | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let selected = $state<StepExecution | null>(null);
	let selectedNode = $state<WorkflowNode | null>(null);
	let selectedInterface = $state<NodeInterface | null>(null);
	let selectedIterations = $state<StepExecution[]>([]);
	let drawerOpen = $state(false);

	const nodesById = $derived.by(() => {
		const map = new Map<string, WorkflowNode>();
		const graph = (template?.graph ?? null) as WorkflowGraph | null;
		if (!graph) return map;
		for (const n of graph.nodes) map.set(n.id, n);
		return map;
	});

	const interfaceRegistry = $derived<InterfaceRegistry>(
		parseInterfaceRegistry(template?.interface_json)
	);

	const stepsByNode = $derived.by(() => {
		const map = new Map<string, StepExecution[]>();
		for (const s of steps) {
			const list = map.get(s.node_id) ?? [];
			list.push(s);
			map.set(s.node_id, list);
		}
		for (const list of map.values()) {
			list.sort((a, b) => a.iteration_index - b.iteration_index);
		}
		return map;
	});

	const isTerminal = $derived(
		instance.status === 'completed' ||
			instance.status === 'failed' ||
			instance.status === 'cancelled'
	);

	async function refresh() {
		try {
			steps = await listStepExecutions(instance.id);
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load step executions';
		} finally {
			loading = false;
		}
	}

	async function loadTemplate() {
		try {
			template = await getTemplate(instance.template_id);
		} catch {
			// Drawer degrades to step-only metadata if the template fetch fails.
		}
	}

	$effect(() => {
		// Reset when the instance changes.
		void instance.id;
		loading = true;
		void loadTemplate();
		refresh();
	});

	// Polling while the instance is live. The projection is eventually
	// consistent (consumer folds events asynchronously) so a tight refresh
	// loop keeps the table close to reality without burning resources once
	// the run terminates.
	$effect(() => {
		if (isTerminal) return;
		const t = setInterval(refresh, 2000);
		return () => clearInterval(t);
	});

	function formatDuration(ms: number | null | undefined): string {
		if (ms === null || ms === undefined) return '—';
		if (ms < 1000) return `${ms}ms`;
		if (ms < 60_000) return `${(ms / 1000).toFixed(2)}s`;
		const mins = Math.floor(ms / 60_000);
		const secs = Math.floor((ms % 60_000) / 1000);
		return `${mins}m ${secs}s`;
	}

	function formatTime(iso: string | null | undefined): string {
		if (!iso) return '—';
		return new Date(iso).toLocaleTimeString();
	}

	function openStep(step: StepExecution) {
		selected = step;
		selectedNode = nodesById.get(step.node_id) ?? null;
		selectedInterface = interfaceRegistry[step.node_id] ?? null;
		selectedIterations = stepsByNode.get(step.node_id) ?? [];
		drawerOpen = true;
	}

	function selectIteration(iterationIndex: number) {
		const found = selectedIterations.find((e) => e.iteration_index === iterationIndex);
		if (found) selected = found;
	}

	function closeDrawer() {
		drawerOpen = false;
		// Keep `selected` so the drawer doesn't reset content during close animation.
	}
</script>

<div class="mx-auto w-full max-w-5xl px-6 py-6">
	{#if loading && steps.length === 0}
		<div class="py-10 text-sm text-muted-foreground text-center">Loading steps…</div>
	{:else if error && steps.length === 0}
		<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{:else if steps.length === 0}
		<div class="py-10 text-sm text-muted-foreground text-center space-y-2">
			<p>No step executions yet.</p>
			<p class="text-sm">
				{#if isTerminal}
					This instance terminated before any step fired.
				{:else}
					Waiting for the projection to catch up — the first events should arrive within a couple of seconds.
				{/if}
			</p>
		</div>
	{:else}
		<div class="rounded-lg border border-border overflow-hidden">
			<table class="w-full text-sm">
				<thead class="bg-muted/30 text-sm uppercase tracking-wide text-muted-foreground">
					<tr>
						<th class="px-4 py-2 text-left font-medium">Step</th>
						<th class="px-4 py-2 text-left font-medium">Kind</th>
						<th class="px-4 py-2 text-left font-medium">Status</th>
						<th class="px-4 py-2 text-left font-medium">Iter</th>
						<th class="px-4 py-2 text-left font-medium">Started</th>
						<th class="px-4 py-2 text-left font-medium">Duration</th>
						<th class="px-4 py-2 text-left font-medium">Output</th>
					</tr>
				</thead>
				<tbody>
					{#each steps as step (step.node_id + '-' + step.iteration_index)}
						<tr
							class="border-t border-border hover:bg-accent cursor-pointer transition-colors"
							onclick={() => openStep(step)}
						>
							<td class="px-4 py-2 font-mono text-sm text-foreground">
								{step.node_id}
							</td>
							<td class="px-4 py-2 text-sm text-muted-foreground">{step.node_kind}</td>
							<td class="px-4 py-2">
								<StatusBadge domain="step" status={step.status} />
							</td>
							<td class="px-4 py-2 text-sm text-muted-foreground">
								{step.iteration_index > 0 ? step.iteration_index : '—'}
							</td>
							<td class="px-4 py-2 text-sm text-muted-foreground">
								{formatTime(step.started_at)}
							</td>
							<td class="px-4 py-2 text-sm text-muted-foreground">
								{formatDuration(step.duration_ms)}
							</td>
							<td class="px-4 py-2 text-sm text-muted-foreground truncate max-w-xs">
								{#if step.outputs && typeof step.outputs === 'object'}
									{Object.keys(step.outputs as Record<string, unknown>).slice(0, 3).join(', ')}
									{#if Object.keys(step.outputs as Record<string, unknown>).length > 3}…{/if}
								{:else if step.branch_taken}
									<span class="font-mono">{step.branch_taken}</span>
								{:else}
									—
								{/if}
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</div>

<StepDetailDrawer
	step={selected}
	node={selectedNode}
	nodeInterface={selectedInterface}
	iterations={selectedIterations}
	instanceId={instance.id}
	open={drawerOpen}
	onClose={closeDrawer}
	onSelectIteration={selectIteration}
/>
