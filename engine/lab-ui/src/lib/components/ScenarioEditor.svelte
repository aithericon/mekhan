<script lang="ts">
	import { multiNetStore } from '$lib/stores/multi-net.svelte';

	let { onClose }: { onClose: () => void } = $props();

	// Define examples using the new port-based semantic format with polymorphic logic
	const EXAMPLES: Record<string, string> = {
		'resource-allocation': JSON.stringify(
			{
				name: 'Resource Allocation',
				description: 'Worker-task assignment system. Workers take tasks, complete them, and return to the pool.',
				places: [
					{
						id: 'workers',
						name: 'Workers',
						type: 'resource',
						initial_tokens: [
							{ worker_id: 'W1', skills: ['general'] },
							{ worker_id: 'W2', skills: ['general'] },
							{ worker_id: 'W3', skills: ['general'] }
						]
					},
					{
						id: 'tasks',
						name: 'Tasks',
						type: 'resource',
						initial_tokens: [
							{ task_id: 'T1', priority: 1 },
							{ task_id: 'T2', priority: 2 },
							{ task_id: 'T3', priority: 3 },
							{ task_id: 'T4', priority: 4 },
							{ task_id: 'T5', priority: 5 }
						]
					},
					{ id: 'in_progress', name: 'In Progress', type: 'state', initial_tokens: [] },
					{ id: 'completed', name: 'Completed', type: 'terminal', initial_tokens: [] }
				],
				transitions: [
					{
						id: 'assign',
						name: 'Assign',
						input_ports: [
							{ name: 'worker', cardinality: 'single' },
							{ name: 'task', cardinality: 'single' }
						],
						output_ports: [{ name: 'work', cardinality: 'single' }],
						inputs: [
							{ place: 'workers', port: 'worker', weight: 1 },
							{ place: 'tasks', port: 'task', weight: 1 }
						],
						outputs: [{ place: 'in_progress', port: 'work', weight: 1 }],
						logic: { type: 'rhai', source: '#{ work: #{ worker: worker, task: task, status: "in_progress" } }' }
					},
					{
						id: 'complete',
						name: 'Complete',
						input_ports: [{ name: 'work', cardinality: 'single' }],
						output_ports: [
							{ name: 'worker_out', cardinality: 'single' },
							{ name: 'done', cardinality: 'single' }
						],
						inputs: [{ place: 'in_progress', port: 'work', weight: 1 }],
						outputs: [
							{ place: 'workers', port: 'worker_out', weight: 1 },
							{ place: 'completed', port: 'done', weight: 1 }
						],
						logic: { type: 'rhai', source: '#{ worker_out: work.worker, done: #{ task: work.task, status: "completed" } }' }
					}
				]
			},
			null,
			2
		),
		'producer-consumer': JSON.stringify(
			{
				name: 'Producer Consumer',
				description: 'Classic producer-consumer pattern with a bounded buffer. Signals trigger production, consumption re-enables producers.',
				places: [
					{
						id: 'ready',
						name: 'Ready to Produce',
						type: 'signal',
						initial_tokens: [{ signal: 1 }, { signal: 2 }, { signal: 3 }]
					},
					{ id: 'buffer', name: 'Buffer', type: 'resource', capacity: 5, initial_tokens: [] },
					{ id: 'consumed', name: 'Consumed', type: 'terminal', initial_tokens: [] }
				],
				transitions: [
					{
						id: 'produce',
						name: 'Produce',
						input_ports: [{ name: 'signal', cardinality: 'single' }],
						output_ports: [{ name: 'item', cardinality: 'single' }],
						inputs: [{ place: 'ready', port: 'signal', weight: 1 }],
						outputs: [{ place: 'buffer', port: 'item', weight: 1 }],
						logic: { type: 'rhai', source: '#{ item: #{ produced_at: "now", from_signal: signal } }' }
					},
					{
						id: 'consume',
						name: 'Consume',
						input_ports: [{ name: 'item', cardinality: 'single' }],
						output_ports: [
							{ name: 'done', cardinality: 'single' },
							{ name: 'ready_signal', cardinality: 'single' }
						],
						inputs: [{ place: 'buffer', port: 'item', weight: 1 }],
						outputs: [
							{ place: 'consumed', port: 'done', weight: 1 },
							{ place: 'ready', port: 'ready_signal', weight: 1 }
						],
						logic: { type: 'rhai', source: '#{ done: #{ consumed: item, status: "done" }, ready_signal: #{ signal: "recycled" } }' }
					}
				]
			},
			null,
			2
		),
		'order-state-machine': JSON.stringify(
			{
				name: 'Order State Machine',
				description: 'Order lifecycle with submit, approve, and reject paths. Guards route based on order amount.',
				places: [
					{
						id: 'pending',
						name: 'Pending',
						type: 'state',
						initial_tokens: [
							{ order_id: 'ORD-001', amount: 150.0, customer: 'Alice' },
							{ order_id: 'ORD-002', amount: 75.0, customer: 'Bob' }
						]
					},
					{ id: 'submitted', name: 'Submitted', type: 'state', initial_tokens: [] },
					{ id: 'approved', name: 'Approved', type: 'terminal', initial_tokens: [] },
					{ id: 'rejected', name: 'Rejected', type: 'terminal', initial_tokens: [] }
				],
				transitions: [
					{
						id: 'submit',
						name: 'Submit',
						input_ports: [{ name: 'order', cardinality: 'single' }],
						output_ports: [{ name: 'submitted_order', cardinality: 'single' }],
						inputs: [{ place: 'pending', port: 'order', weight: 1 }],
						outputs: [{ place: 'submitted', port: 'submitted_order', weight: 1 }],
						logic: { type: 'rhai', source: '#{ submitted_order: #{ order_id: order.order_id, amount: order.amount, customer: order.customer, status: "submitted" } }' }
					},
					{
						id: 'approve',
						name: 'Approve',
						input_ports: [{ name: 'order', cardinality: 'single' }],
						output_ports: [{ name: 'approved_order', cardinality: 'single' }],
						inputs: [{ place: 'submitted', port: 'order', weight: 1 }],
						outputs: [{ place: 'approved', port: 'approved_order', weight: 1 }],
						guard: { type: 'rhai', source: 'order.amount <= 100' },
						logic: { type: 'rhai', source: '#{ approved_order: #{ order_id: order.order_id, amount: order.amount, customer: order.customer, status: "approved", reason: "Auto-approved (amount <= 100)" } }' }
					},
					{
						id: 'reject',
						name: 'Reject',
						input_ports: [{ name: 'order', cardinality: 'single' }],
						output_ports: [{ name: 'rejected_order', cardinality: 'single' }],
						inputs: [{ place: 'submitted', port: 'order', weight: 1 }],
						outputs: [{ place: 'rejected', port: 'rejected_order', weight: 1 }],
						guard: { type: 'rhai', source: 'order.amount > 100' },
						logic: { type: 'rhai', source: '#{ rejected_order: #{ order_id: order.order_id, amount: order.amount, customer: order.customer, status: "rejected", reason: "Requires manual approval (amount > 100)" } }' }
					}
				]
			},
			null,
			2
		),
		'booking-with-retry': JSON.stringify(
			{
				name: 'Booking with Retry',
				description: 'Demonstrates correlation guards and conditional routing. Requests route to success, retry, or fatal based on signal status.',
				places: [
					{
						id: 'requests',
						name: 'Requests',
						type: 'resource',
						initial_tokens: [
							{ id: 'booking-001', customer: 'Alice', room_type: 'deluxe' },
							{ id: 'booking-002', customer: 'Bob', room_type: 'standard' }
						]
					},
					{ id: 'pending', name: 'Pending', type: 'state', initial_tokens: [] },
					{
						id: 'signals',
						name: 'Signals',
						type: 'signal',
						initial_tokens: [{ id: 'booking-001', status: 'OK', resource_id: 'ROOM-42' }]
					},
					{ id: 'completed', name: 'Completed', type: 'terminal', initial_tokens: [] },
					{ id: 'retry_queue', name: 'Retry Queue', type: 'state', initial_tokens: [] },
					{ id: 'failed', name: 'Failed', type: 'terminal', initial_tokens: [] }
				],
				transitions: [
					{
						id: 'submit',
						name: 'Submit',
						input_ports: [{ name: 'request', cardinality: 'single' }],
						output_ports: [{ name: 'context', cardinality: 'single' }],
						inputs: [{ place: 'requests', port: 'request', weight: 1 }],
						outputs: [{ place: 'pending', port: 'context', weight: 1 }],
						logic: { type: 'rhai', source: '#{ context: #{ id: request.id, customer: request.customer, room_type: request.room_type, retry_count: 0, status: "pending" } }' }
					},
					{
						id: 'handle_response',
						name: 'Handle Response',
						input_ports: [
							{ name: 'ctx', cardinality: 'single' },
							{ name: 'signal', cardinality: 'single' }
						],
						output_ports: [
							{ name: 'success', cardinality: 'single' },
							{ name: 'retry', cardinality: 'single' },
							{ name: 'fatal', cardinality: 'single' }
						],
						inputs: [
							{ place: 'pending', port: 'ctx', weight: 1 },
							{ place: 'signals', port: 'signal', weight: 1 }
						],
						outputs: [
							{ place: 'completed', port: 'success', weight: 1 },
							{ place: 'retry_queue', port: 'retry', weight: 1 },
							{ place: 'failed', port: 'fatal', weight: 1 }
						],
						guard: { type: 'rhai', source: 'ctx.id == signal.id' },
						logic: { type: 'rhai', source: 'if signal.status == "OK" { #{ success: #{ id: ctx.id, customer: ctx.customer, resource: signal.resource_id, status: "completed" } } } else if ctx.retry_count < 3 { #{ retry: #{ id: ctx.id, customer: ctx.customer, room_type: ctx.room_type, retry_count: ctx.retry_count + 1, status: "retrying" } } } else { #{ fatal: #{ id: ctx.id, customer: ctx.customer, error: "Max retries exceeded", status: "failed" } } }' }
					},
					{
						id: 'resubmit',
						name: 'Resubmit',
						input_ports: [{ name: 'retry_ctx', cardinality: 'single' }],
						output_ports: [{ name: 'pending_ctx', cardinality: 'single' }],
						inputs: [{ place: 'retry_queue', port: 'retry_ctx', weight: 1 }],
						outputs: [{ place: 'pending', port: 'pending_ctx', weight: 1 }],
						logic: { type: 'rhai', source: '#{ pending_ctx: retry_ctx }' }
					}
				]
			},
			null,
			2
		),
		'resilient-job-lifecycle': JSON.stringify(
			{
				name: 'Resilient Job Lifecycle',
				description:
					'Lease pattern with bounded retries and fatal error handling. Errors include a fatal flag - transient errors requeue (up to max_retries), fatal errors fail immediately.',
				places: [
					{
						id: 'p_queue',
						name: 'Job Queue',
						type: 'resource',
						initial_tokens: [
							{ id: 'job-1', max_retries: 3, retries: 0 },
							{ id: 'job-2', max_retries: 2, retries: 0 }
						]
					},
					{
						id: 'p_pool',
						name: 'Worker Pool',
						type: 'resource',
						initial_tokens: [{ id: 'gpu-1' }, { id: 'gpu-2' }]
					},
					{ id: 'p_reserved', name: 'Reserved (Pending Ack)', type: 'state', initial_tokens: [] },
					{ id: 'p_sig_reserved', name: 'Sig: Reserved', type: 'signal', initial_tokens: [] },
					{ id: 'p_sig_reserve_error', name: 'Sig: Reserve Error', type: 'signal', initial_tokens: [] },
					{ id: 'p_running', name: 'Running', type: 'state', initial_tokens: [] },
					{ id: 'p_sig_completed', name: 'Sig: Completed', type: 'signal', initial_tokens: [] },
					{ id: 'p_sig_exec_error', name: 'Sig: Exec Error', type: 'signal', initial_tokens: [] },
					{ id: 'p_sig_cancelled', name: 'Sig: Cancelled', type: 'signal', initial_tokens: [] },
					{ id: 'p_done', name: 'Done', type: 'terminal', initial_tokens: [] },
					{ id: 'p_failed', name: 'Failed', type: 'terminal', initial_tokens: [] }
				],
				transitions: [
					{
						id: 't_reserve',
						name: '1. Reserve (Prepare)',
						input_ports: [{ name: 'job' }, { name: 'worker' }],
						output_ports: [{ name: 'reservation' }],
						inputs: [
							{ place: 'p_queue', port: 'job' },
							{ place: 'p_pool', port: 'worker' }
						],
						outputs: [{ place: 'p_reserved', port: 'reservation' }],
						logic: { type: 'rhai', source: '#{ reservation: #{ job_id: job.id, worker_id: worker.id, max_retries: job.max_retries, retries: job.retries } }' }
					},
					{
						id: 't_confirm_lease',
						name: '2a. Confirm Lease',
						input_ports: [{ name: 'reservation' }, { name: 'sig' }],
						output_ports: [{ name: 'lease' }],
						inputs: [
							{ place: 'p_reserved', port: 'reservation' },
							{ place: 'p_sig_reserved', port: 'sig' }
						],
						outputs: [{ place: 'p_running', port: 'lease' }],
						guard: { type: 'rhai', source: 'reservation.job_id == sig.correlation_id' },
						logic: { type: 'rhai', source: '#{ lease: #{ job_id: reservation.job_id, worker_id: reservation.worker_id, max_retries: reservation.max_retries, retries: reservation.retries } }' }
					},
					{
						id: 't_rollback',
						name: '2b. Rollback (Retry)',
						input_ports: [{ name: 'reservation' }, { name: 'sig' }],
						output_ports: [{ name: 'job' }, { name: 'worker' }],
						inputs: [
							{ place: 'p_reserved', port: 'reservation' },
							{ place: 'p_sig_reserve_error', port: 'sig' }
						],
						outputs: [
							{ place: 'p_queue', port: 'job' },
							{ place: 'p_pool', port: 'worker' }
						],
						guard: { type: 'rhai', source: 'reservation.job_id == sig.correlation_id && reservation.retries < reservation.max_retries' },
						logic: { type: 'rhai', source: '#{ job: #{ id: reservation.job_id, max_retries: reservation.max_retries, retries: reservation.retries + 1 }, worker: #{ id: reservation.worker_id } }' }
					},
					{
						id: 't_rollback_exhausted',
						name: '2c. Reserve Fail (Exhausted)',
						input_ports: [{ name: 'reservation' }, { name: 'sig' }],
						output_ports: [{ name: 'fail' }, { name: 'worker' }],
						inputs: [
							{ place: 'p_reserved', port: 'reservation' },
							{ place: 'p_sig_reserve_error', port: 'sig' }
						],
						outputs: [
							{ place: 'p_failed', port: 'fail' },
							{ place: 'p_pool', port: 'worker' }
						],
						guard: { type: 'rhai', source: 'reservation.job_id == sig.correlation_id && reservation.retries >= reservation.max_retries' },
						logic: { type: 'rhai', source: '#{ fail: #{ id: reservation.job_id, error: "Reservation retries exhausted" }, worker: #{ id: reservation.worker_id } }' }
					},
					{
						id: 't_finish',
						name: '3a. Complete OK',
						input_ports: [{ name: 'lease' }, { name: 'sig' }],
						output_ports: [{ name: 'result' }, { name: 'worker' }],
						inputs: [
							{ place: 'p_running', port: 'lease' },
							{ place: 'p_sig_completed', port: 'sig' }
						],
						outputs: [
							{ place: 'p_done', port: 'result' },
							{ place: 'p_pool', port: 'worker' }
						],
						guard: { type: 'rhai', source: 'lease.job_id == sig.correlation_id' },
						logic: { type: 'rhai', source: '#{ result: #{ id: lease.job_id, output: sig.data }, worker: #{ id: lease.worker_id } }' }
					},
					{
						id: 't_requeue',
						name: '3b. Error (Requeue)',
						input_ports: [{ name: 'lease' }, { name: 'sig' }],
						output_ports: [{ name: 'job' }, { name: 'worker' }],
						inputs: [
							{ place: 'p_running', port: 'lease' },
							{ place: 'p_sig_exec_error', port: 'sig' }
						],
						outputs: [
							{ place: 'p_queue', port: 'job' },
							{ place: 'p_pool', port: 'worker' }
						],
						guard: { type: 'rhai', source: 'lease.job_id == sig.correlation_id && !sig.fatal && lease.retries < lease.max_retries' },
						logic: { type: 'rhai', source: '#{ job: #{ id: lease.job_id, max_retries: lease.max_retries, retries: lease.retries + 1 }, worker: #{ id: lease.worker_id } }' }
					},
					{
						id: 't_exhausted',
						name: '3c. Error (Exhausted)',
						input_ports: [{ name: 'lease' }, { name: 'sig' }],
						output_ports: [{ name: 'fail' }, { name: 'worker' }],
						inputs: [
							{ place: 'p_running', port: 'lease' },
							{ place: 'p_sig_exec_error', port: 'sig' }
						],
						outputs: [
							{ place: 'p_failed', port: 'fail' },
							{ place: 'p_pool', port: 'worker' }
						],
						guard: { type: 'rhai', source: 'lease.job_id == sig.correlation_id && !sig.fatal && lease.retries >= lease.max_retries' },
						logic: { type: 'rhai', source: '#{ fail: #{ id: lease.job_id, error: "Retries exhausted" }, worker: #{ id: lease.worker_id } }' }
					},
					{
						id: 't_fatal',
						name: '3d. Fatal Error',
						input_ports: [{ name: 'lease' }, { name: 'sig' }],
						output_ports: [{ name: 'fail' }, { name: 'worker' }],
						inputs: [
							{ place: 'p_running', port: 'lease' },
							{ place: 'p_sig_exec_error', port: 'sig' }
						],
						outputs: [
							{ place: 'p_failed', port: 'fail' },
							{ place: 'p_pool', port: 'worker' }
						],
						guard: { type: 'rhai', source: 'lease.job_id == sig.correlation_id && sig.fatal' },
						logic: { type: 'rhai', source: '#{ fail: #{ id: lease.job_id, error: sig.error }, worker: #{ id: lease.worker_id } }' }
					},
					{
						id: 't_cancel_reserved',
						name: 'Cancel (Reserved)',
						input_ports: [{ name: 'reservation' }, { name: 'sig' }],
						output_ports: [{ name: 'fail' }, { name: 'worker' }],
						inputs: [
							{ place: 'p_reserved', port: 'reservation' },
							{ place: 'p_sig_cancelled', port: 'sig' }
						],
						outputs: [
							{ place: 'p_failed', port: 'fail' },
							{ place: 'p_pool', port: 'worker' }
						],
						guard: { type: 'rhai', source: 'reservation.job_id == sig.correlation_id' },
						logic: { type: 'rhai', source: '#{ fail: #{ id: reservation.job_id, reason: "User Cancel" }, worker: #{ id: reservation.worker_id } }' }
					},
					{
						id: 't_cancel_running',
						name: 'Cancel (Running)',
						input_ports: [{ name: 'lease' }, { name: 'sig' }],
						output_ports: [{ name: 'fail' }, { name: 'worker' }],
						inputs: [
							{ place: 'p_running', port: 'lease' },
							{ place: 'p_sig_cancelled', port: 'sig' }
						],
						outputs: [
							{ place: 'p_failed', port: 'fail' },
							{ place: 'p_pool', port: 'worker' }
						],
						guard: { type: 'rhai', source: 'lease.job_id == sig.correlation_id' },
						logic: { type: 'rhai', source: '#{ fail: #{ id: lease.job_id, reason: "User Cancel" }, worker: #{ id: lease.worker_id } }' }
					}
				],
				mock_adapters: [
					{
						name: 'Resource Scheduler',
						trigger_place_id: 'p_reserved',
						latency_ms: 500,
						logic: {
							type: 'rhai',
							source:
								'let r = random(); if r < 0.8 { #{ target_place: "p_sig_reserved", data: #{ correlation_id: token.job_id } } } else { #{ target_place: "p_sig_reserve_error", data: #{ correlation_id: token.job_id, error: "Resource busy" } } }'
						}
					},
					{
						name: 'HPC Worker',
						trigger_place_id: 'p_running',
						latency_ms: 2000,
						logic: {
							type: 'rhai',
							source:
								'let r = random(); if r < 0.7 { #{ target_place: "p_sig_completed", data: #{ correlation_id: token.job_id, data: "Result_" + timestamp() } } } else if r < 0.9 { #{ target_place: "p_sig_exec_error", data: #{ correlation_id: token.job_id, error: "Network timeout", fatal: false } } } else { #{ target_place: "p_sig_exec_error", data: #{ correlation_id: token.job_id, error: "Hardware failure", fatal: true } } }'
						}
					}
				]
			},
			null,
			2
		),
		'bridge-effects-demo': JSON.stringify(
			{
				name: 'Order Fulfillment with External Notification',
				description:
					'Demonstrates bridge places and effect transitions. Orders flow through validation and fulfillment, then bridge out to a warehouse net. A reply inbox receives confirmations, and an effect transition sends customer notifications.',
				places: [
					{
						id: 'orders',
						name: 'Orders',
						type: 'state',
						initial_tokens: [
							{ order_id: 'ORD-100', item: 'Widget A', qty: 5 },
							{ order_id: 'ORD-101', item: 'Gadget B', qty: 2 }
						]
					},
					{ id: 'validated', name: 'Validated', type: 'state', initial_tokens: [] },
					{ id: 'fulfilled', name: 'Fulfilled', type: 'state', initial_tokens: [] },
					{
						id: 'outbox',
						name: 'Warehouse Outbox',
						type: 'state',
						initial_tokens: [],
						bridge_out: {
							target_net_id: 'warehouse-net',
							target_place_name: 'incoming',
							reply_to: 'reply_inbox'
						}
					},
					{
						id: 'reply_inbox',
						name: 'Reply Inbox',
						type: 'state',
						initial_tokens: [],
						bridge_reply: true
					},
					{ id: 'completed', name: 'Completed', type: 'terminal', initial_tokens: [] }
				],
				transitions: [
					{
						id: 'validate',
						name: 'Validate',
						input_ports: [{ name: 'order', cardinality: 'single' }],
						output_ports: [{ name: 'valid', cardinality: 'single' }],
						inputs: [{ place: 'orders', port: 'order', weight: 1 }],
						outputs: [{ place: 'validated', port: 'valid', weight: 1 }],
						logic: {
							type: 'rhai',
							source:
								'#{ valid: #{ order_id: order.order_id, item: order.item, qty: order.qty, status: "validated" } }'
						}
					},
					{
						id: 'fulfill',
						name: 'Fulfill',
						input_ports: [{ name: 'order', cardinality: 'single' }],
						output_ports: [
							{ name: 'done', cardinality: 'single' },
							{ name: 'ship_request', cardinality: 'single' }
						],
						inputs: [{ place: 'validated', port: 'order', weight: 1 }],
						outputs: [
							{ place: 'fulfilled', port: 'done', weight: 1 },
							{ place: 'outbox', port: 'ship_request', weight: 1 }
						],
						logic: {
							type: 'rhai',
							source:
								'#{ done: #{ order_id: order.order_id, status: "fulfilled" }, ship_request: #{ order_id: order.order_id, item: order.item, qty: order.qty } }'
						}
					},
					{
						id: 'notify_customer',
						name: 'Notify Customer',
						input_ports: [
							{ name: 'confirmation', cardinality: 'single' },
							{ name: 'order', cardinality: 'single' }
						],
						output_ports: [{ name: 'notified', cardinality: 'single' }],
						inputs: [
							{ place: 'reply_inbox', port: 'confirmation', weight: 1 },
							{ place: 'fulfilled', port: 'order', weight: 1 }
						],
						outputs: [{ place: 'completed', port: 'notified', weight: 1 }],
						logic: { type: 'effect', handler_id: 'send_notification' }
					}
				]
			},
			null,
			2
		),
		'cross-net-a': JSON.stringify(
			{
				name: 'Cross-Net Producer (Net A)',
				places: [
					{
						id: 'source',
						name: 'Source',
						type: 'state',
						initial_tokens: [{ Data: { msg: 'hello from net-a' } }]
					},
					{
						id: 'outbox',
						name: 'Request Outbox (bridge-out)',
						type: 'state',
						initial_tokens: [],
						bridge_out: {
							target_net_id: 'net-b',
							target_place_name: 'inbox',
							reply_to: 'reply_inbox'
						}
					},
					{
						id: 'reply_inbox',
						name: 'Reply Inbox',
						type: 'state',
						bridge_reply: true
					}
				],
				transitions: [
					{
						id: 'produce',
						name: 'Produce',
						input_ports: [{ name: 'input', cardinality: 'single' }],
						output_ports: [{ name: 'output', cardinality: 'single' }],
						inputs: [{ place: 'source', port: 'input', weight: 1 }],
						outputs: [{ place: 'outbox', port: 'output', weight: 1 }],
						logic: { type: 'rhai', source: '#{output: input}' }
					}
				]
			},
			null,
			2
		),
		'cross-net-b': JSON.stringify(
			{
				name: 'Cross-Net Consumer (Net B)',
				places: [
					{
						id: 'inbox',
						name: 'Inbox (bridge-in)',
						type: 'state',
						initial_tokens: []
					},
					{
						id: 'processed',
						name: 'Processed',
						type: 'state',
						initial_tokens: []
					},
					{
						id: 'reply_outbox',
						name: 'Reply Outbox',
						type: 'state',
						bridge_reply: true
					}
				],
				transitions: [
					{
						id: 'process',
						name: 'Process',
						input_ports: [{ name: 'input', cardinality: 'single' }],
						output_ports: [
							{ name: 'result', cardinality: 'single' },
							{ name: 'reply', cardinality: 'single' }
						],
						inputs: [{ place: 'inbox', port: 'input', weight: 1 }],
						outputs: [
							{ place: 'processed', port: 'result', weight: 1 },
							{ place: 'reply_outbox', port: 'reply', weight: 1 }
						],
						logic: {
							type: 'rhai',
							source: '#{result: input, reply: #{status: "done", original: input}}'
						}
					}
				]
			},
			null,
			2
		),
		'nomad-batch': JSON.stringify(
			{
				name: 'nomad-batch',
				description:
					'Nomad batch job net with signal-based completion, retry, and dead-letter routing',
				places: [
					{
						id: 'job_queue',
						name: 'Job Queue',
						type: 'state',
						initial_tokens: [
							{ job_id: 'batch-001', max_retries: 3, retries: 0, run: 0, task_name: 'data-preprocess' },
							{ job_id: 'batch-002', max_retries: 2, retries: 0, run: 0, task_name: 'model-training' },
							{ job_id: 'batch-003', max_retries: 1, retries: 0, run: 0, task_name: 'evaluation' }
						],
						token_schema: '#/definitions/BatchJob'
					},
					{ id: 'submitted_jobs', name: 'Submitted Jobs', type: 'state', token_schema: '#/definitions/SubmittedJob' },
					{ id: 'sig_running', name: 'Running Signals', type: 'signal', token_schema: '#/definitions/DynamicToken' },
					{ id: 'running_jobs', name: 'Running Jobs', type: 'state', token_schema: '#/definitions/SubmittedJob' },
					{ id: 'sig_completed', name: 'Completed Signals', type: 'signal', token_schema: '#/definitions/DynamicToken' },
					{ id: 'sig_failed', name: 'Failed Signals', type: 'signal', token_schema: '#/definitions/DynamicToken' },
					{ id: 'completed', name: 'Completed Jobs', type: 'state', token_schema: '#/definitions/CompletedJob' },
					{ id: 'failed_jobs', name: 'Failed Jobs', type: 'state', token_schema: '#/definitions/FailedJob' },
					{ id: 'effect_errors', name: 'Effect Errors', type: 'state', token_schema: '#/definitions/DynamicToken' },
					{ id: 'dead_letter', name: 'Dead Letter', type: 'state', token_schema: '#/definitions/DeadLetter' }
				],
				transitions: [
					{
						id: 'submit_job',
						name: 'Submit to Nomad',
						input_ports: [{ name: 'job', schema_ref: '#/definitions/BatchJob', cardinality: 'single' }],
						output_ports: [
							{ name: 'submitted', schema_ref: '#/definitions/SubmittedJob', cardinality: 'single' },
							{ name: '_error', schema_ref: '#/definitions/DynamicToken', cardinality: 'single' }
						],
						inputs: [{ place: 'job_queue', port: 'job', weight: 1 }],
						outputs: [
							{ place: 'submitted_jobs', port: 'submitted', weight: 1 },
							{ place: 'effect_errors', port: '_error', weight: 1 }
						],
						logic: { type: 'effect', handler_id: 'scheduler_submit' }
					},
					{
						id: 't_running',
						name: 'Job Running',
						input_ports: [
							{ name: 'job', schema_ref: '#/definitions/SubmittedJob', cardinality: 'single' },
							{ name: 'sig', schema_ref: '#/definitions/DynamicToken', cardinality: 'single' }
						],
						output_ports: [{ name: 'running', schema_ref: '#/definitions/SubmittedJob', cardinality: 'single' }],
						inputs: [
							{ place: 'submitted_jobs', port: 'job', weight: 1 },
							{ place: 'sig_running', port: 'sig', weight: 1 }
						],
						outputs: [{ place: 'running_jobs', port: 'running', weight: 1 }],
						guard: { type: 'rhai', source: 'sig.scheduler_job_id == job.scheduler_job_id' },
						logic: { type: 'rhai', source: '#{ running: job }' }
					},
					{
						id: 't_success',
						name: 'Job Completed',
						input_ports: [
							{ name: 'job', schema_ref: '#/definitions/SubmittedJob', cardinality: 'single' },
							{ name: 'sig', schema_ref: '#/definitions/DynamicToken', cardinality: 'single' }
						],
						output_ports: [{ name: 'done', schema_ref: '#/definitions/CompletedJob', cardinality: 'single' }],
						inputs: [
							{ place: 'running_jobs', port: 'job', weight: 1 },
							{ place: 'sig_completed', port: 'sig', weight: 1 }
						],
						outputs: [{ place: 'completed', port: 'done', weight: 1 }],
						guard: { type: 'rhai', source: 'sig.scheduler_job_id == job.scheduler_job_id' },
						logic: {
							type: 'rhai',
							source: '#{\n    done: #{\n        job_id: job.job_id,\n        task_name: job.task_name,\n        scheduler_job_id: job.scheduler_job_id,\n        exit_code: sig.exit_code,\n        node_name: sig.node_name\n    }\n}'
						}
					},
					{
						id: 't_failed',
						name: 'Job Failed',
						input_ports: [
							{ name: 'job', schema_ref: '#/definitions/SubmittedJob', cardinality: 'single' },
							{ name: 'sig', schema_ref: '#/definitions/DynamicToken', cardinality: 'single' }
						],
						output_ports: [{ name: 'err', schema_ref: '#/definitions/FailedJob', cardinality: 'single' }],
						inputs: [
							{ place: 'running_jobs', port: 'job', weight: 1 },
							{ place: 'sig_failed', port: 'sig', weight: 1 }
						],
						outputs: [{ place: 'failed_jobs', port: 'err', weight: 1 }],
						guard: { type: 'rhai', source: 'sig.scheduler_job_id == job.scheduler_job_id' },
						logic: {
							type: 'rhai',
							source: '#{\n    err: #{\n        job_id: job.job_id,\n        task_name: job.task_name,\n        scheduler_job_id: job.scheduler_job_id,\n        exit_code: sig.exit_code,\n        message: sig.message,\n        retries: job.retries,\n        max_retries: job.max_retries,\n        run: job.run\n    }\n}'
						}
					},
					{
						id: 'retry',
						name: 'Retry Failed Job',
						input_ports: [{ name: 'err', schema_ref: '#/definitions/FailedJob', cardinality: 'single' }],
						output_ports: [{ name: 'job', schema_ref: '#/definitions/BatchJob', cardinality: 'single' }],
						inputs: [{ place: 'failed_jobs', port: 'err', weight: 1 }],
						outputs: [{ place: 'job_queue', port: 'job', weight: 1 }],
						guard: { type: 'rhai', source: 'err.retries < err.max_retries' },
						logic: {
							type: 'rhai',
							source: '#{\n    job: #{\n        job_id: err.job_id,\n        task_name: err.task_name,\n        run: err.run + 1,\n        retries: err.retries + 1,\n        max_retries: err.max_retries\n    }\n}'
						}
					},
					{
						id: 'dead_letter',
						name: 'Dead Letter',
						input_ports: [{ name: 'err', schema_ref: '#/definitions/FailedJob', cardinality: 'single' }],
						output_ports: [{ name: 'dead', schema_ref: '#/definitions/DeadLetter', cardinality: 'single' }],
						inputs: [{ place: 'failed_jobs', port: 'err', weight: 1 }],
						outputs: [{ place: 'dead_letter', port: 'dead', weight: 1 }],
						guard: { type: 'rhai', source: 'err.retries >= err.max_retries' },
						logic: {
							type: 'rhai',
							source: '#{\n    dead: #{\n        job_id: err.job_id,\n        task_name: err.task_name,\n        last_error: err.message,\n        retries_exhausted: err.retries\n    }\n}'
						}
					},
					{
						id: 'retry_effect_err',
						name: 'Retry Effect Error',
						input_ports: [{ name: 'err', schema_ref: '#/definitions/DynamicToken', cardinality: 'single' }],
						output_ports: [{ name: 'job', schema_ref: '#/definitions/BatchJob', cardinality: 'single' }],
						inputs: [{ place: 'effect_errors', port: 'err', weight: 1 }],
						outputs: [{ place: 'job_queue', port: 'job', weight: 1 }],
						guard: { type: 'rhai', source: 'err.retryable == true' },
						logic: { type: 'rhai', source: '#{ job: err.inputs.job }' }
					},
					{
						id: 'dlq_effect_err',
						name: 'Dead Letter Effect Error',
						input_ports: [{ name: 'err', schema_ref: '#/definitions/DynamicToken', cardinality: 'single' }],
						output_ports: [{ name: 'dead', schema_ref: '#/definitions/DeadLetter', cardinality: 'single' }],
						inputs: [{ place: 'effect_errors', port: 'err', weight: 1 }],
						outputs: [{ place: 'dead_letter', port: 'dead', weight: 1 }],
						guard: { type: 'rhai', source: 'err.retryable != true' },
						logic: {
							type: 'rhai',
							source: '#{\n    dead: #{\n        job_id: err.inputs.job.job_id,\n        task_name: err.inputs.job.task_name,\n        last_error: err.error,\n        retries_exhausted: 0\n    }\n}'
						}
					}
				],
				definitions: {
					BatchJob: {
						$schema: 'http://json-schema.org/draft-07/schema#',
						title: 'BatchJob',
						type: 'object',
						required: ['job_id', 'max_retries', 'retries', 'run', 'task_name'],
						properties: {
							job_id: { type: 'string' },
							max_retries: { type: 'integer', format: 'int64' },
							retries: { type: 'integer', format: 'int64' },
							run: { type: 'integer', format: 'int64' },
							task_name: { type: 'string' }
						}
					},
					SubmittedJob: {
						$schema: 'http://json-schema.org/draft-07/schema#',
						title: 'SubmittedJob',
						type: 'object',
						required: ['job_id', 'max_retries', 'retries', 'run', 'scheduler_job_id', 'task_name'],
						properties: {
							job_id: { type: 'string' },
							max_retries: { type: 'integer', format: 'int64' },
							retries: { type: 'integer', format: 'int64' },
							run: { type: 'integer', format: 'int64' },
							scheduler_job_id: { type: 'string' },
							task_name: { type: 'string' }
						}
					},
					CompletedJob: {
						$schema: 'http://json-schema.org/draft-07/schema#',
						title: 'CompletedJob',
						type: 'object',
						required: ['exit_code', 'job_id', 'node_name', 'scheduler_job_id', 'task_name'],
						properties: {
							exit_code: { type: 'integer', format: 'int64' },
							job_id: { type: 'string' },
							node_name: { type: 'string' },
							scheduler_job_id: { type: 'string' },
							task_name: { type: 'string' }
						}
					},
					FailedJob: {
						$schema: 'http://json-schema.org/draft-07/schema#',
						title: 'FailedJob',
						type: 'object',
						required: ['exit_code', 'job_id', 'max_retries', 'message', 'retries', 'run', 'scheduler_job_id', 'task_name'],
						properties: {
							exit_code: { type: 'integer', format: 'int64' },
							job_id: { type: 'string' },
							max_retries: { type: 'integer', format: 'int64' },
							message: { type: 'string' },
							retries: { type: 'integer', format: 'int64' },
							run: { type: 'integer', format: 'int64' },
							scheduler_job_id: { type: 'string' },
							task_name: { type: 'string' }
						}
					},
					DeadLetter: {
						$schema: 'http://json-schema.org/draft-07/schema#',
						title: 'DeadLetter',
						type: 'object',
						required: ['job_id', 'last_error', 'retries_exhausted', 'task_name'],
						properties: {
							job_id: { type: 'string' },
							last_error: { type: 'string' },
							retries_exhausted: { type: 'integer', format: 'int64' },
							task_name: { type: 'string' }
						}
					},
					DynamicToken: {
						$schema: 'http://json-schema.org/draft-07/schema#',
						title: 'DynamicToken',
						description: 'Dynamic token - untyped JSON data.'
					}
				}
			},
			null,
			2
		),
		'analysis-showcase': JSON.stringify(
			{
				name: 'Analysis Showcase',
				description:
					'Demonstrates static analysis features: Terminal places, disconnected ports, unreachable states, dead ends, and finite resources. Load this to see various validation issues.',
				places: [
					{
						id: 'start',
						name: 'Start',
						type: 'state',
						initial_tokens: [{ id: 'item-1' }, { id: 'item-2' }]
					},
					{
						id: 'processing',
						name: 'Processing',
						type: 'state',
						initial_tokens: []
					},
					{
						id: 'complete',
						name: 'Complete',
						type: 'terminal',
						initial_tokens: []
					},
					{
						id: 'orphan_state',
						name: 'Orphan State',
						type: 'state',
						initial_tokens: []
					},
					{
						id: 'dead_end',
						name: 'Dead End',
						type: 'state',
						initial_tokens: [{ stuck: true }]
					},
					{
						id: 'finite_pool',
						name: 'Finite Pool',
						type: 'resource',
						initial_tokens: [{ resource_id: 'R1' }]
					},
					{
						id: 'unused_signal',
						name: 'Unused Signal',
						type: 'signal',
						initial_tokens: [{ signal: 'alert' }]
					}
				],
				transitions: [
					{
						id: 'process',
						name: 'Process',
						input_ports: [{ name: 'item', cardinality: 'single' }],
						output_ports: [{ name: 'processed', cardinality: 'single' }],
						inputs: [{ place: 'start', port: 'item', weight: 1 }],
						outputs: [{ place: 'processing', port: 'processed', weight: 1 }],
						logic: { type: 'rhai', source: '#{ processed: #{ id: item.id, status: "processing" } }' }
					},
					{
						id: 'finalize',
						name: 'Finalize',
						input_ports: [{ name: 'work', cardinality: 'single' }],
						output_ports: [{ name: 'done', cardinality: 'single' }],
						inputs: [{ place: 'processing', port: 'work', weight: 1 }],
						outputs: [{ place: 'complete', port: 'done', weight: 1 }],
						logic: { type: 'rhai', source: '#{ done: #{ id: work.id, status: "complete" } }' }
					},
					{
						id: 'broken_transition',
						name: 'Broken Transition',
						input_ports: [
							{ name: 'connected', cardinality: 'single' },
							{ name: 'disconnected', cardinality: 'single' }
						],
						output_ports: [{ name: 'out', cardinality: 'single' }],
						inputs: [{ place: 'dead_end', port: 'connected', weight: 1 }],
						outputs: [{ place: 'orphan_state', port: 'out', weight: 1 }],
						logic: { type: 'rhai', source: '#{ out: connected }' }
					},
					{
						id: 'leaky_transition',
						name: 'Leaky Transition',
						input_ports: [{ name: 'input', cardinality: 'single' }],
						output_ports: [
							{ name: 'main', cardinality: 'single' },
							{ name: 'unused_out', cardinality: 'single' }
						],
						inputs: [{ place: 'finite_pool', port: 'input', weight: 1 }],
						outputs: [{ place: 'dead_end', port: 'main', weight: 1 }],
						logic: { type: 'rhai', source: '#{ main: input }' }
					}
				]
			},
			null,
			2
		)
	};

	const DEFAULT_SCENARIO = EXAMPLES['resource-allocation'];

	// State variables
	let scenarioJson = $state(DEFAULT_SCENARIO);
	let targetNetId = $state('default');
	let loading = $state(false);
	let error = $state<string | null>(null);
	let success = $state<string | null>(null);

	/** Load a scenario into the target net. */
	async function loadScenario() {
		loading = true;
		error = null;
		success = null;

		try {
			const scenario = JSON.parse(scenarioJson);

			// Ensure the target net tab exists; create store if needed
			const store = multiNetStore.addNet(targetNetId);

			const result = await store.loadScenario(scenario);
			if (result.success) {
				success = `Loaded into "${targetNetId}": ${result.places_count} places, ${result.transitions_count} transitions, ${result.tokens_count} tokens`;
				// Refresh data
				await store.fetchTopology();
				await store.fetchEvents();
				await store.fetchAnalysis();
				// Switch to the loaded net
				multiNetStore.setActive(targetNetId);
				// Close after short delay
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

	/** Load the cross-net pair simultaneously into net-a and net-b. */
	async function loadCrossNetPair() {
		loading = true;
		error = null;
		success = null;

		try {
			const scenarioA = JSON.parse(EXAMPLES['cross-net-a']);
			const scenarioB = JSON.parse(EXAMPLES['cross-net-b']);

			// Create/get stores for both nets
			const storeA = multiNetStore.addNet('net-a', 'net-a');
			const storeB = multiNetStore.addNet('net-b', 'net-b');

			// Load both scenarios in parallel
			const [resultA, resultB] = await Promise.all([
				storeA.loadScenario(scenarioA),
				storeB.loadScenario(scenarioB)
			]);

			if (resultA.success && resultB.success) {
				// Refresh both
				await Promise.all([
					storeA.fetchTopology(),
					storeA.fetchEvents(),
					storeA.fetchAnalysis(),
					storeB.fetchTopology(),
					storeB.fetchEvents(),
					storeB.fetchAnalysis(),
				]);
				success = `Loaded Cross-Net pair: net-a (${resultA.places_count} places) + net-b (${resultB.places_count} places)`;
				multiNetStore.setActive('net-a');
				setTimeout(() => onClose(), 1500);
			} else {
				error = `net-a: ${resultA.error ?? 'ok'}, net-b: ${resultB.error ?? 'ok'}`;
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

<div id="scenario-editor-modal" class="fixed inset-0 bg-black/50 flex items-center justify-center z-50" role="dialog">
	<div id="scenario-editor" class="bg-card rounded-lg shadow-xl w-[900px] max-h-[90vh] flex flex-col">
		<!-- Header -->
		<div class="px-4 py-3 border-b flex items-center justify-between">
			<h2 class="text-lg font-semibold">Load Scenario</h2>
			<button onclick={onClose} class="text-muted-foreground hover:text-foreground text-xl">&times;</button>
		</div>

		<!-- Example buttons -->
		<div class="px-4 py-2 border-b bg-muted flex gap-2 flex-wrap">
			<span class="text-sm text-muted-foreground mr-2">Examples:</span>
			<button
				onclick={() => loadExample('resource-allocation')}
				class="px-2 py-1 text-xs bg-blue-500/15 hover:bg-blue-500/25 text-blue-400 rounded"
			>
				Resource Allocation
			</button>
			<button
				onclick={() => loadExample('producer-consumer')}
				class="px-2 py-1 text-xs bg-green-500/15 hover:bg-green-500/25 text-green-400 rounded"
			>
				Producer-Consumer
			</button>
			<button
				onclick={() => loadExample('order-state-machine')}
				class="px-2 py-1 text-xs bg-yellow-500/15 hover:bg-yellow-500/25 text-yellow-400 rounded"
			>
				Order (Guards)
			</button>
			<button
				onclick={() => loadExample('booking-with-retry')}
				class="px-2 py-1 text-xs bg-purple-500/15 hover:bg-purple-500/25 text-purple-400 rounded"
			>
				Booking (Retry)
			</button>
			<button
				onclick={() => loadExample('resilient-job-lifecycle')}
				class="px-2 py-1 text-xs bg-cyan-500/15 hover:bg-cyan-500/25 text-cyan-400 rounded"
			>
				Resilient Job (Lease)
			</button>
			<button
				onclick={() => loadExample('bridge-effects-demo')}
				class="px-2 py-1 text-xs bg-rose-500/15 hover:bg-rose-500/25 text-rose-400 rounded"
			>
				Bridge + Effects
			</button>
			<button
				onclick={() => loadExample('cross-net-a')}
				class="px-2 py-1 text-xs bg-rose-500/15 hover:bg-rose-500/25 text-rose-400 rounded"
			>
				Cross-Net A
			</button>
			<button
				onclick={() => loadExample('cross-net-b')}
				class="px-2 py-1 text-xs bg-indigo-500/15 hover:bg-indigo-500/25 text-indigo-400 rounded"
			>
				Cross-Net B
			</button>
			<button
				onclick={() => loadExample('nomad-batch')}
				class="px-2 py-1 text-xs bg-orange-500/15 hover:bg-orange-500/25 text-orange-400 rounded"
			>
				Nomad Batch
			</button>
			<button
				onclick={() => loadExample('analysis-showcase')}
				class="px-2 py-1 text-xs bg-red-500/15 hover:bg-red-500/25 text-red-400 rounded"
			>
				Analysis Demo
			</button>
		</div>

		<!-- Net target + Cross-net pair -->
		<div class="px-4 py-2 border-b bg-muted flex items-center gap-3">
			<label class="text-sm text-muted-foreground flex items-center gap-2">
				Load into net:
				<input
					type="text"
					bind:value={targetNetId}
					class="px-2 py-1 text-sm border rounded w-32 bg-background"
					placeholder="default"
				/>
			</label>
			<button
				onclick={loadCrossNetPair}
				disabled={loading}
				class="px-3 py-1 text-xs bg-gradient-to-r from-rose-500 to-indigo-500 text-white rounded hover:from-rose-400 hover:to-indigo-400 disabled:opacity-50"
			>
				Load Cross-Net Pair (net-a + net-b)
			</button>
		</div>

		<!-- Editor -->
		<div class="flex-1 overflow-hidden p-4">
			<textarea
				id="scenario-json-editor"
				bind:value={scenarioJson}
				class="w-full h-[400px] font-mono text-sm p-3 border rounded bg-gray-900 text-green-400 resize-none"
				spellcheck="false"
			></textarea>
		</div>

		<!-- Status messages -->
		{#if error}
			<div id="scenario-error" class="px-4 py-2 bg-red-500/15 text-red-400 text-sm">{error}</div>
		{/if}
		{#if success}
			<div id="scenario-success" class="px-4 py-2 bg-green-500/15 text-green-400 text-sm">{success}</div>
		{/if}

		<!-- Footer -->
		<div class="px-4 py-3 border-t flex justify-end gap-2">
			<button id="btn-cancel" onclick={onClose} class="px-4 py-2 text-sm text-muted-foreground hover:text-foreground">
				Cancel
			</button>
			<button
				id="btn-load-scenario-confirm"
				onclick={loadScenario}
				disabled={loading}
				class="px-4 py-2 text-sm bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50"
			>
				{loading ? 'Loading...' : 'Load Scenario'}
			</button>
		</div>
	</div>
</div>
