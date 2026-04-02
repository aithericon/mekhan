<script lang="ts">
	import type { PetriStore } from '$lib/stores/petri.svelte';

	interface Props {
		store: PetriStore;
		netId: string;
		onClose: () => void;
	}

	let { store, netId, onClose }: Props = $props();

	const EXAMPLES: Record<string, string> = {
		'resource-allocation': JSON.stringify(
			{
				name: 'Resource Allocation',
				description: 'Worker-task assignment system.',
				places: [
					{ id: 'workers', name: 'Workers', type: 'resource', initial_tokens: [{ worker_id: 'W1', skills: ['general'] }, { worker_id: 'W2', skills: ['general'] }, { worker_id: 'W3', skills: ['general'] }] },
					{ id: 'tasks', name: 'Tasks', type: 'resource', initial_tokens: [{ task_id: 'T1', priority: 1 }, { task_id: 'T2', priority: 2 }, { task_id: 'T3', priority: 3 }, { task_id: 'T4', priority: 4 }, { task_id: 'T5', priority: 5 }] },
					{ id: 'in_progress', name: 'In Progress', type: 'state', initial_tokens: [] },
					{ id: 'completed', name: 'Completed', type: 'terminal', initial_tokens: [] }
				],
				transitions: [
					{ id: 'assign', name: 'Assign', input_ports: [{ name: 'worker', cardinality: 'single' }, { name: 'task', cardinality: 'single' }], output_ports: [{ name: 'work', cardinality: 'single' }], inputs: [{ place: 'workers', port: 'worker', weight: 1 }, { place: 'tasks', port: 'task', weight: 1 }], outputs: [{ place: 'in_progress', port: 'work', weight: 1 }], logic: { type: 'rhai', source: '#{ work: #{ worker: worker, task: task, status: "in_progress" } }' } },
					{ id: 'complete', name: 'Complete', input_ports: [{ name: 'work', cardinality: 'single' }], output_ports: [{ name: 'worker_out', cardinality: 'single' }, { name: 'done', cardinality: 'single' }], inputs: [{ place: 'in_progress', port: 'work', weight: 1 }], outputs: [{ place: 'workers', port: 'worker_out', weight: 1 }, { place: 'completed', port: 'done', weight: 1 }], logic: { type: 'rhai', source: '#{ worker_out: work.worker, done: #{ task: work.task, status: "completed" } }' } }
				]
			},
			null, 2
		),
		'producer-consumer': JSON.stringify(
			{
				name: 'Producer Consumer',
				description: 'Classic producer-consumer pattern with a bounded buffer.',
				places: [
					{ id: 'ready', name: 'Ready to Produce', type: 'signal', initial_tokens: [{ signal: 1 }, { signal: 2 }, { signal: 3 }] },
					{ id: 'buffer', name: 'Buffer', type: 'resource', capacity: 5, initial_tokens: [] },
					{ id: 'consumed', name: 'Consumed', type: 'terminal', initial_tokens: [] }
				],
				transitions: [
					{ id: 'produce', name: 'Produce', input_ports: [{ name: 'signal', cardinality: 'single' }], output_ports: [{ name: 'item', cardinality: 'single' }], inputs: [{ place: 'ready', port: 'signal', weight: 1 }], outputs: [{ place: 'buffer', port: 'item', weight: 1 }], logic: { type: 'rhai', source: '#{ item: #{ produced_at: "now", from_signal: signal } }' } },
					{ id: 'consume', name: 'Consume', input_ports: [{ name: 'item', cardinality: 'single' }], output_ports: [{ name: 'done', cardinality: 'single' }, { name: 'ready_signal', cardinality: 'single' }], inputs: [{ place: 'buffer', port: 'item', weight: 1 }], outputs: [{ place: 'consumed', port: 'done', weight: 1 }, { place: 'ready', port: 'ready_signal', weight: 1 }], logic: { type: 'rhai', source: '#{ done: #{ consumed: item, status: "done" }, ready_signal: #{ signal: "recycled" } }' } }
				]
			},
			null, 2
		),
		'order-state-machine': JSON.stringify(
			{
				name: 'Order State Machine',
				description: 'Order lifecycle with submit, approve, and reject paths. Guards route based on order amount.',
				places: [
					{ id: 'pending', name: 'Pending', type: 'state', initial_tokens: [{ order_id: 'ORD-001', amount: 150.0, customer: 'Alice' }, { order_id: 'ORD-002', amount: 75.0, customer: 'Bob' }] },
					{ id: 'submitted', name: 'Submitted', type: 'state', initial_tokens: [] },
					{ id: 'approved', name: 'Approved', type: 'terminal', initial_tokens: [] },
					{ id: 'rejected', name: 'Rejected', type: 'terminal', initial_tokens: [] }
				],
				transitions: [
					{ id: 'submit', name: 'Submit', input_ports: [{ name: 'order', cardinality: 'single' }], output_ports: [{ name: 'submitted_order', cardinality: 'single' }], inputs: [{ place: 'pending', port: 'order', weight: 1 }], outputs: [{ place: 'submitted', port: 'submitted_order', weight: 1 }], logic: { type: 'rhai', source: '#{ submitted_order: #{ order_id: order.order_id, amount: order.amount, customer: order.customer, status: "submitted" } }' } },
					{ id: 'approve', name: 'Approve', input_ports: [{ name: 'order', cardinality: 'single' }], output_ports: [{ name: 'approved_order', cardinality: 'single' }], inputs: [{ place: 'submitted', port: 'order', weight: 1 }], outputs: [{ place: 'approved', port: 'approved_order', weight: 1 }], guard: { type: 'rhai', source: 'order.amount <= 100' }, logic: { type: 'rhai', source: '#{ approved_order: #{ order_id: order.order_id, amount: order.amount, customer: order.customer, status: "approved", reason: "Auto-approved (amount <= 100)" } }' } },
					{ id: 'reject', name: 'Reject', input_ports: [{ name: 'order', cardinality: 'single' }], output_ports: [{ name: 'rejected_order', cardinality: 'single' }], inputs: [{ place: 'submitted', port: 'order', weight: 1 }], outputs: [{ place: 'rejected', port: 'rejected_order', weight: 1 }], guard: { type: 'rhai', source: 'order.amount > 100' }, logic: { type: 'rhai', source: '#{ rejected_order: #{ order_id: order.order_id, amount: order.amount, customer: order.customer, status: "rejected", reason: "Requires manual approval (amount > 100)" } }' } }
				]
			},
			null, 2
		),
		'booking-with-retry': JSON.stringify(
			{
				name: 'Booking with Retry',
				description: 'Demonstrates correlation guards and conditional routing.',
				places: [
					{ id: 'requests', name: 'Requests', type: 'resource', initial_tokens: [{ id: 'booking-001', customer: 'Alice', room_type: 'deluxe' }, { id: 'booking-002', customer: 'Bob', room_type: 'standard' }] },
					{ id: 'pending', name: 'Pending', type: 'state', initial_tokens: [] },
					{ id: 'signals', name: 'Signals', type: 'signal', initial_tokens: [{ id: 'booking-001', status: 'OK', resource_id: 'ROOM-42' }] },
					{ id: 'completed', name: 'Completed', type: 'terminal', initial_tokens: [] },
					{ id: 'retry_queue', name: 'Retry Queue', type: 'state', initial_tokens: [] },
					{ id: 'failed', name: 'Failed', type: 'terminal', initial_tokens: [] }
				],
				transitions: [
					{ id: 'submit', name: 'Submit', input_ports: [{ name: 'request', cardinality: 'single' }], output_ports: [{ name: 'context', cardinality: 'single' }], inputs: [{ place: 'requests', port: 'request', weight: 1 }], outputs: [{ place: 'pending', port: 'context', weight: 1 }], logic: { type: 'rhai', source: '#{ context: #{ id: request.id, customer: request.customer, room_type: request.room_type, retry_count: 0, status: "pending" } }' } },
					{ id: 'handle_response', name: 'Handle Response', input_ports: [{ name: 'ctx', cardinality: 'single' }, { name: 'signal', cardinality: 'single' }], output_ports: [{ name: 'success', cardinality: 'single' }, { name: 'retry', cardinality: 'single' }, { name: 'fatal', cardinality: 'single' }], inputs: [{ place: 'pending', port: 'ctx', weight: 1 }, { place: 'signals', port: 'signal', weight: 1 }], outputs: [{ place: 'completed', port: 'success', weight: 1 }, { place: 'retry_queue', port: 'retry', weight: 1 }, { place: 'failed', port: 'fatal', weight: 1 }], guard: { type: 'rhai', source: 'ctx.id == signal.id' }, logic: { type: 'rhai', source: 'if signal.status == "OK" { #{ success: #{ id: ctx.id, customer: ctx.customer, resource: signal.resource_id, status: "completed" } } } else if ctx.retry_count < 3 { #{ retry: #{ id: ctx.id, customer: ctx.customer, room_type: ctx.room_type, retry_count: ctx.retry_count + 1, status: "retrying" } } } else { #{ fatal: #{ id: ctx.id, customer: ctx.customer, error: "Max retries exceeded", status: "failed" } } }' } },
					{ id: 'resubmit', name: 'Resubmit', input_ports: [{ name: 'retry_ctx', cardinality: 'single' }], output_ports: [{ name: 'pending_ctx', cardinality: 'single' }], inputs: [{ place: 'retry_queue', port: 'retry_ctx', weight: 1 }], outputs: [{ place: 'pending', port: 'pending_ctx', weight: 1 }], logic: { type: 'rhai', source: '#{ pending_ctx: retry_ctx }' } }
				]
			},
			null, 2
		)
	};

	const DEFAULT_SCENARIO = EXAMPLES['resource-allocation'];

	let scenarioJson = $state(DEFAULT_SCENARIO);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let success = $state<string | null>(null);

	async function loadScenario() {
		loading = true;
		error = null;
		success = null;

		try {
			const scenario = JSON.parse(scenarioJson);
			const result = await store.loadScenario(scenario);
			if (result.success) {
				success = `Loaded into "${netId}": ${result.places_count} places, ${result.transitions_count} transitions, ${result.tokens_count} tokens`;
				// Refresh store data
				await store.reset();
				setTimeout(() => onClose(), 1500);
			} else {
				error = result.error ?? 'Unknown error';
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Invalid JSON';
		} finally {
			loading = false;
		}
	}

	function loadExample(name: string) {
		scenarioJson = EXAMPLES[name] ?? DEFAULT_SCENARIO;
	}
</script>

<div class="fixed inset-0 bg-black/50 flex items-center justify-center z-50" role="dialog">
	<div class="bg-card rounded-lg shadow-xl w-[900px] max-h-[90vh] flex flex-col">
		<!-- Header -->
		<div class="px-4 py-3 border-b flex items-center justify-between">
			<h2 class="text-lg font-semibold">Load Scenario</h2>
			<button onclick={onClose} class="text-muted-foreground hover:text-foreground text-xl">&times;</button>
		</div>

		<!-- Example buttons -->
		<div class="px-4 py-2 border-b bg-muted flex gap-2 flex-wrap">
			<span class="text-sm text-muted-foreground mr-2">Examples:</span>
			<button onclick={() => loadExample('resource-allocation')} class="px-2 py-1 text-xs bg-blue-500/15 hover:bg-blue-500/25 text-blue-400 rounded">
				Resource Allocation
			</button>
			<button onclick={() => loadExample('producer-consumer')} class="px-2 py-1 text-xs bg-green-500/15 hover:bg-green-500/25 text-green-400 rounded">
				Producer-Consumer
			</button>
			<button onclick={() => loadExample('order-state-machine')} class="px-2 py-1 text-xs bg-yellow-500/15 hover:bg-yellow-500/25 text-yellow-400 rounded">
				Order (Guards)
			</button>
			<button onclick={() => loadExample('booking-with-retry')} class="px-2 py-1 text-xs bg-purple-500/15 hover:bg-purple-500/25 text-purple-400 rounded">
				Booking (Retry)
			</button>
		</div>

		<!-- Target net -->
		<div class="px-4 py-2 border-b bg-muted flex items-center gap-3">
			<label class="text-sm text-muted-foreground flex items-center gap-2">
				Target net:
				<span class="px-2 py-1 text-sm font-mono border rounded bg-background">{netId}</span>
			</label>
		</div>

		<!-- Editor -->
		<div class="flex-1 overflow-hidden p-4">
			<textarea
				bind:value={scenarioJson}
				class="w-full h-[400px] font-mono text-sm p-3 border rounded bg-gray-900 text-green-400 resize-none"
				spellcheck="false"
			></textarea>
		</div>

		<!-- Status messages -->
		{#if error}
			<div class="px-4 py-2 bg-red-500/15 text-red-400 text-sm">{error}</div>
		{/if}
		{#if success}
			<div class="px-4 py-2 bg-green-500/15 text-green-400 text-sm">{success}</div>
		{/if}

		<!-- Footer -->
		<div class="px-4 py-3 border-t flex justify-end gap-2">
			<button onclick={onClose} class="px-4 py-2 text-sm text-muted-foreground hover:text-foreground">
				Cancel
			</button>
			<button
				onclick={loadScenario}
				disabled={loading}
				class="px-4 py-2 text-sm bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50"
			>
				{loading ? 'Loading...' : 'Load Scenario'}
			</button>
		</div>
	</div>
</div>
