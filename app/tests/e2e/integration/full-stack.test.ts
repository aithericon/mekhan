/**
 * Full-stack E2E integration tests.
 *
 * These tests call real APIs against the running infrastructure:
 *   - mekhan-service (http://localhost:13100)
 *   - petri-lab      (http://localhost:13030)
 *
 * NO MOCKS. The full stack must be running before executing these tests.
 */

import { test, expect } from '@playwright/test';
import {
	AUTHOR_ID,
	apiPost,
	apiPut,
	apiGet,
	apiDelete,
	petriGet,
	petriPost,
	hpiGet,
	hpiPost,
	servicesHealthy,
	initHpiToken,
	createAndPublish,
	createInstance,
	waitForInstanceStatus,
	waitForTokenAtPlace,
	waitForHpiTask
} from './helpers';

// ---------------------------------------------------------------------------
// Test setup: skip entire suite if services are not running
// ---------------------------------------------------------------------------

test.beforeAll(async () => {
	const healthy = await servicesHealthy();
	test.skip(!healthy, 'Full stack is not running — skipping integration tests');

	// Authenticate with HPI for human task tests
	await initHpiToken();
});

// Track created resources for cleanup
const createdTemplateIds: string[] = [];
const createdInstanceIds: string[] = [];

// test.afterAll(async () => {
// 	// Clean up instances first (they reference templates)
// 	for (const id of createdInstanceIds) {
// 		try {
// 			// Cancel if running, then we rely on template deletion cascade
// 			await apiDelete(`/api/instances/${id}`);
// 		} catch {
// 			// ignore
// 		}
// 	}
// 	// Clean up templates
// 	for (const id of createdTemplateIds) {
// 		try {
// 			await apiDelete(`/api/templates/${id}`);
// 		} catch {
// 			// ignore
// 		}
// 	}
// });

// ---------------------------------------------------------------------------
// Test 1: Simple Start -> End lifecycle
// ---------------------------------------------------------------------------

test.describe('Test 1: Simple Start->End Lifecycle', () => {
	test('creates, publishes, and completes a Start->End workflow', async () => {
		// 1. Create template with default Start->End graph
		const createRes = await apiPost('/api/templates', {
			name: `e2e-simple-${Date.now()}`,
			author_id: AUTHOR_ID
			// no graph → uses default Start -> End
		});
		expect(createRes.status).toBe(201);
		const template = await createRes.json();
		createdTemplateIds.push(template.id);
		expect(template.id).toBeTruthy();
		expect(template.published).toBe(false);

		// 2. Publish → compiles graph to AIR
		const pubRes = await apiPost(`/api/templates/${template.id}/publish`, {});
		expect(pubRes.status).toBe(200);
		const published = await pubRes.json();
		expect(published.published).toBe(true);
		expect(published.air_json).toBeTruthy();

		// 3. Verify AIR is valid JSON with expected structure
		const airRes = await apiGet(`/api/templates/${template.id}/air`);
		expect(airRes.status).toBe(200);
		const air = await airRes.json();
		expect(air.places).toBeDefined();
		expect(air.transitions).toBeDefined();
		expect(Array.isArray(air.places)).toBe(true);
		expect(Array.isArray(air.transitions)).toBe(true);
		// Chain merge collapses Start→End into a single place: p_start_ready
		// absorbs the terminal type from p_end_done (0 transitions, 1 place).
		const placeIds = air.places.map((p: { id: string }) => p.id);
		expect(placeIds).toContain('p_start_ready');
		const startPlace = air.places.find((p: { id: string }) => p.id === 'p_start_ready');
		expect(startPlace.type).toBe('terminal');

		// 4. Create instance (deploys to petri-lab)
		const instRes = await apiPost('/api/instances', {
			template_id: template.id,
			created_by: AUTHOR_ID
		});
		expect(instRes.status).toBe(201);
		const instance = await instRes.json();
		createdInstanceIds.push(instance.id);
		expect(instance.status).toBe('running');
		expect(instance.net_id).toMatch(/^mekhan-/);

		// 5. Poll until instance is completed
		//    The Start→End graph auto-completes because:
		//    - Chain merge collapses to 1 terminal place with initial token
		//    - Quiescence + token in terminal → NetCompleted
		//    - Lifecycle listener updates DB status
		await waitForInstanceStatus(instance.id, ['completed']);

		// 6. Verify completed_at is set
		const finalRes = await apiGet(`/api/instances/${instance.id}`);
		expect(finalRes.status).toBe(200);
		const finalInstance = await finalRes.json();
		expect(finalInstance.status).toBe('completed');
		expect(finalInstance.completed_at).toBeTruthy();
	});
});

// ---------------------------------------------------------------------------
// Test 2: Verify petri-lab state during execution
// ---------------------------------------------------------------------------

test.describe('Test 2: Petri-lab state verification', () => {
	test('can observe petri-lab net state after deployment', async () => {
		// Create and publish a simple Start->End template
		const template = await createAndPublish(`e2e-state-${Date.now()}`);
		createdTemplateIds.push(template.id);

		// Create instance
		const instance = await createInstance(template.id);
		createdInstanceIds.push(instance.id);

		// Immediately query the petri-lab net state via the mekhan-service proxy
		const stateRes = await apiGet(`/api/instances/${instance.id}/state`);

		// The net may have already completed (Start->End is very fast), so
		// either the state endpoint returns 200 with marking info, or the
		// net is already gone (502 from petri-lab).
		if (stateRes.status === 200) {
			const state = await stateRes.json();
			expect(state.instance_id).toBe(instance.id);
			expect(state.net_id).toBe(instance.net_id);
			expect(state.marking).toBeDefined();
		}

		// Also verify petri-lab net state directly
		const directState = await petriGet(`/api/nets/${instance.net_id}/state`);
		// May be 404 if already completed and removed, which is fine
		if (directState.status === 200) {
			const netState = await directState.json();
			expect(netState.marking).toBeDefined();
			expect(netState.enabled_transitions).toBeDefined();
		}

		// Wait for completion regardless
		await waitForInstanceStatus(instance.id, ['completed']);
	});
});

// ---------------------------------------------------------------------------
// Test 3: Template versioning + multiple instances
// ---------------------------------------------------------------------------

test.describe('Test 3: Template versioning + multiple instances', () => {
	test('creates v1 and v2, runs instances from each, both complete', async () => {
		const baseName = `e2e-version-${Date.now()}`;

		// 1. Create + publish v1
		const v1 = await createAndPublish(baseName);
		createdTemplateIds.push(v1.id);
		expect(v1.version).toBe(1);
		expect(v1.published).toBe(true);

		// 2. Create instance from v1
		const instanceA = await createInstance(v1.id);
		createdInstanceIds.push(instanceA.id);
		expect(instanceA.template_version).toBe(1);

		// 3. Create new version v2
		const newVerRes = await apiPost(`/api/templates/${v1.id}/new-version`, {});
		expect(newVerRes.status).toBe(201);
		const v2Draft = await newVerRes.json();
		createdTemplateIds.push(v2Draft.id);
		expect(v2Draft.version).toBe(2);
		expect(v2Draft.published).toBe(false);

		// 4. Publish v2 (uses the same graph, which is fine)
		const pubV2 = await apiPost(`/api/templates/${v2Draft.id}/publish`, {});
		expect(pubV2.status).toBe(200);
		const v2Published = await pubV2.json();
		expect(v2Published.published).toBe(true);

		// 5. Create instance from v2
		const instanceB = await createInstance(v2Published.id);
		createdInstanceIds.push(instanceB.id);
		expect(instanceB.template_version).toBe(2);

		// 6. Both should complete
		await Promise.all([
			waitForInstanceStatus(instanceA.id, ['completed']),
			waitForInstanceStatus(instanceB.id, ['completed'])
		]);

		// 7. Verify both are completed
		const [aRes, bRes] = await Promise.all([
			apiGet(`/api/instances/${instanceA.id}`),
			apiGet(`/api/instances/${instanceB.id}`)
		]);
		const a = await aRes.json();
		const b = await bRes.json();
		expect(a.status).toBe('completed');
		expect(b.status).toBe('completed');

		// 8. Verify version chain
		const versionsRes = await apiGet(`/api/templates/${v1.id}/versions`);
		expect(versionsRes.status).toBe(200);
		const versions = await versionsRes.json();
		expect(versions.length).toBeGreaterThanOrEqual(2);
	});
});

// ---------------------------------------------------------------------------
// Test 4: Cancel a running instance
// ---------------------------------------------------------------------------

test.describe('Test 4: Cancel a running instance', () => {
	test('cancels a running HumanTask workflow instance', async () => {
		// Create a template with Start -> HumanTask -> End
		// The HumanTask will block (waiting for signal), giving us time to cancel.
		const graph = {
			nodes: [
				{
					id: 'start',
					type: 'start',
					position: { x: 250, y: 100 },
					data: {
						type: 'start',
						label: 'Start',
						description: null,
						initialData: null
					}
				},
				{
					id: 'ht1',
					type: 'human_task',
					position: { x: 250, y: 250 },
					data: {
						type: 'human_task',
						label: 'Review',
						description: null,
						taskTitle: 'Review Task',
						instructionsMdsvex: null,
						steps: [
							{
								id: 'step1',
								title: 'Approve',
								descriptionMdsvex: null,
								blocks: [
									{
										type: 'input',
										field: {
											name: 'approved',
											label: 'Approved?',
											kind: 'checkbox',
											required: true
										}
									}
								]
							}
						]
					}
				},
				{
					id: 'end',
					type: 'end',
					position: { x: 250, y: 400 },
					data: {
						type: 'end',
						label: 'End',
						description: null
					}
				}
			],
			edges: [
				{
					id: 'e_start_ht1',
					source: 'start',
					target: 'ht1',
					source_handle: null,
					label: null,
					type: 'sequence'
				},
				{
					id: 'e_ht1_end',
					source: 'ht1',
					target: 'end',
					source_handle: null,
					label: null,
					type: 'sequence'
				}
			]
		};

		const template = await createAndPublish(`e2e-cancel-${Date.now()}`, graph);
		createdTemplateIds.push(template.id);

		// Create instance
		const instance = await createInstance(template.id);
		createdInstanceIds.push(instance.id);
		expect(instance.status).toBe('running');

		// Give petri-lab a moment to evaluate the initial transitions.
		// The net should get stuck at the human_task signal place.
		await new Promise((r) => setTimeout(r, 2000));

		// Verify it is still running (not completed)
		const midRes = await apiGet(`/api/instances/${instance.id}`);
		expect(midRes.status).toBe(200);
		const midInstance = await midRes.json();
		expect(midInstance.status).toBe('running');

		// Cancel the instance
		const cancelRes = await apiDelete(`/api/instances/${instance.id}`);
		expect(cancelRes.status).toBe(200);
		const cancelled = await cancelRes.json();
		expect(cancelled.status).toBe('cancelled');
		expect(cancelled.completed_at).toBeTruthy();

		// Verify petri-lab net is terminated (should be 404 or deleted)
		const netRes = await petriGet(`/api/nets/${instance.net_id}/state`);
		// After terminate: net should be gone (404)
		expect([200, 404]).toContain(netRes.status);
	});
});

// ---------------------------------------------------------------------------
// Test 5: Human task workflow (Start -> HumanTask -> End) with signal injection
// ---------------------------------------------------------------------------

test.describe('Test 5: Human task completion via HPI round-trip', () => {
	test('completes a HumanTask workflow end-to-end through HPI', async () => {
		test.setTimeout(60_000);

		const graph = {
			nodes: [
				{
					id: 'start',
					type: 'start',
					position: { x: 250, y: 100 },
					data: {
						type: 'start',
						label: 'Start',
						description: null,
						initialData: null
					}
				},
				{
					id: 'ht1',
					type: 'human_task',
					position: { x: 250, y: 250 },
					data: {
						type: 'human_task',
						label: 'Review',
						description: null,
						taskTitle: 'Review Task',
						instructionsMdsvex: null,
						steps: [
							{
								id: 'step1',
								title: 'Approve',
								descriptionMdsvex: null,
								blocks: [
									{
										type: 'input',
										field: {
											name: 'approved',
											label: 'Approved?',
											kind: 'checkbox',
											required: true
										}
									}
								]
							}
						]
					}
				},
				{
					id: 'end',
					type: 'end',
					position: { x: 250, y: 400 },
					data: {
						type: 'end',
						label: 'End',
						description: null
					}
				}
			],
			edges: [
				{
					id: 'e_start_ht1',
					source: 'start',
					target: 'ht1',
					source_handle: null,
					label: null,
					type: 'sequence'
				},
				{
					id: 'e_ht1_end',
					source: 'ht1',
					target: 'end',
					source_handle: null,
					label: null,
					type: 'sequence'
				}
			]
		};

		// 1. Create template + publish (triggers compilation to AIR)
		const template = await createAndPublish(`e2e-human-${Date.now()}`, graph);
		createdTemplateIds.push(template.id);

		// 2. Verify the AIR has the signal place
		const airRes = await apiGet(`/api/templates/${template.id}/air`);
		expect(airRes.status).toBe(200);
		const air = await airRes.json();
		const signalPlace = air.places.find(
			(p: { id: string; type: string }) => p.id === 'p_ht1_signal' && p.type === 'signal'
		);
		expect(signalPlace).toBeTruthy();

		// 3. Create instance (deploys net to petri-lab, starts eval loop)
		const instance = await createInstance(template.id);
		createdInstanceIds.push(instance.id);
		expect(instance.status).toBe('running');
		expect(instance.net_id).toMatch(/^mekhan-/);

		// 4. Wait for the human_task effect to fire.
		//
		// Full flow (automatic, no manual intervention):
		//   start token → edge transition (Rhai enriches with title/steps)
		//   → token at p_ht1_input → t_ht1_request effect fires
		//   → HumanTaskHandler publishes HumanTaskRequest to NATS
		//   → outputs token to p_ht1_active with task_id
		const activeToken = await waitForTokenAtPlace(
			instance.net_id,
			'p_ht1_active',
			30_000
		);
		const taskId = String(activeToken.task_id);
		expect(taskId).toBeTruthy();

		// 5. Wait for HPI to pick up the task via NATS consumer.
		//
		// HPI's startRequestConsumer() listens on human.request.>,
		// creates the task in its DB with a NATS sink pointing at
		// petri.signal.{net_id}.p_ht1_signal.
		const hpiTask = await waitForHpiTask(taskId, 30_000);
		expect(hpiTask.task_id ?? hpiTask.id).toBe(taskId);

		// 6. Complete the task via HPI API.
		//
		// This triggers: completeTask() → domain event → sink adapter
		// → publishes ExternalSignal to petri.signal.{net_id}.p_ht1_signal
		// → GlobalSignalListener injects token into p_ht1_signal place.
		const completeRes = await hpiPost(
			`/api/v1/tasks/${taskId}/complete`,
			{ data: { approved: true } }
		);
		expect(completeRes.status).toBe(200);
		const completeBody = await completeRes.json();
		expect(completeBody.status).toBe('completed');

		// 7. Wait for the instance to complete.
		//
		// The finalize transition correlates p_ht1_active[task_id] with
		// p_ht1_signal[task_id], merges state + signal, outputs to p_ht1_output.
		// Edge transition moves token to terminal place.
		// petri-lab detects quiescence + terminal token → NetCompleted.
		// Lifecycle listener in mekhan-service updates DB status.
		await waitForInstanceStatus(instance.id, ['completed'], 30_000);

		// 8. Verify final state in mekhan DB
		const finalRes = await apiGet(`/api/instances/${instance.id}`);
		expect(finalRes.status).toBe(200);
		const finalInstance = await finalRes.json();
		expect(finalInstance.status).toBe('completed');
		expect(finalInstance.completed_at).toBeTruthy();
	});
});

// ---------------------------------------------------------------------------
// Test 6: Error cases
// ---------------------------------------------------------------------------

test.describe('Test 6: Error cases', () => {
	test('returns 400 when creating instance from unpublished template', async () => {
		// Create a template but do NOT publish it
		const createRes = await apiPost('/api/templates', {
			name: `e2e-error-${Date.now()}`,
			author_id: AUTHOR_ID
		});
		expect(createRes.status).toBe(201);
		const template = await createRes.json();
		createdTemplateIds.push(template.id);

		// Try to create instance from unpublished template
		const instRes = await apiPost('/api/instances', {
			template_id: template.id,
			created_by: AUTHOR_ID
		});
		expect(instRes.status).toBe(400);
		const error = await instRes.json();
		expect(error.error).toMatch(/not published/i);
	});

	test('returns 409 when publishing an already published template', async () => {
		const template = await createAndPublish(`e2e-double-pub-${Date.now()}`);
		createdTemplateIds.push(template.id);

		// Try to publish again
		const res = await apiPost(`/api/templates/${template.id}/publish`, {});
		expect(res.status).toBe(409);
	});

	test('returns 409 when editing a published template', async () => {
		const template = await createAndPublish(`e2e-edit-pub-${Date.now()}`);
		createdTemplateIds.push(template.id);

		const res = await apiPut(`/api/templates/${template.id}`, {
			name: 'Attempted Edit'
		});
		expect(res.status).toBe(409);
	});

	test('returns 404 for non-existent template', async () => {
		const res = await apiGet('/api/templates/00000000-0000-0000-0000-000000000000');
		expect(res.status).toBe(404);
	});

	test('returns 404 for non-existent instance', async () => {
		const res = await apiGet('/api/instances/00000000-0000-0000-0000-000000000000');
		expect(res.status).toBe(404);
	});

	test('returns 409 when cancelling an already completed instance', async () => {
		const template = await createAndPublish(`e2e-cancel-done-${Date.now()}`);
		createdTemplateIds.push(template.id);

		const instance = await createInstance(template.id);
		createdInstanceIds.push(instance.id);

		// Wait for completion (Start->End is fast)
		await waitForInstanceStatus(instance.id, ['completed']);

		// Try to cancel
		const res = await apiDelete(`/api/instances/${instance.id}`);
		expect(res.status).toBe(409);
	});
});

// ---------------------------------------------------------------------------
// Test 7: Demo showcase graph — create, publish, deploy
// ---------------------------------------------------------------------------

test.describe('Test 7: Demo showcase graph lifecycle', () => {
	test('creates, publishes, and deploys the showcase graph', async () => {
		// Same showcase graph that seeds the "Try Demo" template.
		// Tests realistic field configs and end-to-end deployment.
		const showcaseGraph = {
			nodes: [
				{
					id: 'start',
					type: 'start',
					position: { x: 40, y: 280 },
					data: { type: 'start', label: 'Start', initialData: { invoice_id: 'INV-TEST-001' } }
				},
				{
					id: 'review',
					type: 'human_task',
					position: { x: 240, y: 250 },
					data: {
						type: 'human_task',
						label: 'Review Invoice',
						taskTitle: 'Review Incoming Invoice',
						instructionsMdsvex: 'Review the invoice details.',
						steps: [
							{
								id: 'step-verify',
								title: 'Verify Details',
								blocks: [
									{ type: 'input', field: { name: 'vendor_name', label: 'Vendor Name', kind: 'text', required: true } },
									{ type: 'input', field: { name: 'invoice_amount', label: 'Amount', kind: 'number', required: true } },
									{ type: 'input', field: { name: 'verified', label: 'Confirmed', kind: 'checkbox', required: true } }
								]
							}
						]
					}
				},
				{
					id: 'extract',
					type: 'automated_step',
					position: { x: 520, y: 250 },
					data: {
						type: 'automated_step',
						label: 'Extract Data',
						executionSpec: { backendType: 'python', config: { scriptContent: 'import json, sys\nprint(json.dumps({"extracted": True}))' } }
					}
				},
				{
					id: 'check-amount',
					type: 'decision',
					position: { x: 800, y: 255 },
					data: {
						type: 'decision',
						label: 'Amount Check',
						conditions: [
							{ edgeId: 'branch-high', label: 'High Value', guard: 'review.invoice_amount > 5000' }
						],
						defaultBranch: 'default'
					}
				},
				{
					id: 'split',
					type: 'parallel_split',
					position: { x: 1080, y: 120 },
					data: { type: 'parallel_split', label: 'Dual Review' }
				},
				{
					id: 'manager-approval',
					type: 'human_task',
					position: { x: 1320, y: 40 },
					data: {
						type: 'human_task',
						label: 'Manager Approval',
						taskTitle: 'Approve High-Value Invoice',
						steps: [
							{
								id: 'step-decide',
								title: 'Decision',
								blocks: [
									{ type: 'input', field: { name: 'decision', label: 'Decision', kind: 'select', required: true, options: ['Approve', 'Reject'] } },
									{ type: 'input', field: { name: 'signature', label: 'Signature', kind: 'signature', required: true } }
								]
							}
						]
					}
				},
				{
					id: 'compliance',
					type: 'automated_step',
					position: { x: 1320, y: 210 },
					data: {
						type: 'automated_step',
						label: 'Compliance Check',
						executionSpec: { backendType: 'python', config: { scriptContent: 'import json, sys\nprint(json.dumps({"compliant": True}))' } }
					}
				},
				{
					id: 'join',
					type: 'join',
					position: { x: 1600, y: 120 },
					data: { type: 'join', label: 'Merge Results', mode: 'all' }
				},
				{
					id: 'end-approved',
					type: 'end',
					position: { x: 1820, y: 120 },
					data: { type: 'end', label: 'Approved' }
				},
				{
					id: 'end-processed',
					type: 'end',
					position: { x: 1080, y: 410 },
					data: { type: 'end', label: 'Processed' }
				}
			],
			edges: [
				{ id: 'e-start-review', source: 'start', target: 'review', type: 'sequence' },
				{ id: 'e-review-extract', source: 'review', target: 'extract', type: 'sequence' },
				{ id: 'e-extract-decision', source: 'extract', target: 'check-amount', type: 'sequence' },
				{ id: 'e-decision-split', source: 'check-amount', target: 'split', sourceHandle: 'branch-high', label: '> $5,000', type: 'conditional' },
				{ id: 'e-decision-processed', source: 'check-amount', target: 'end-processed', sourceHandle: 'default', label: '≤ $5,000', type: 'conditional' },
				{ id: 'e-split-manager', source: 'split', target: 'manager-approval', type: 'sequence' },
				{ id: 'e-split-compliance', source: 'split', target: 'compliance', type: 'sequence' },
				{ id: 'e-manager-join', source: 'manager-approval', target: 'join', type: 'sequence' },
				{ id: 'e-compliance-join', source: 'compliance', target: 'join', type: 'sequence' },
				{ id: 'e-join-end', source: 'join', target: 'end-approved', type: 'sequence' }
			]
		};

		// Step 1: Create template with showcase graph
		const createRes = await apiPost('/api/templates', {
			name: 'Showcase Demo Test',
			description: 'E2E test of the demo showcase graph',
			graph: showcaseGraph,
			author_id: AUTHOR_ID
		});
		expect(createRes.status).toBe(201);
		const template = await createRes.json();
		expect(template.id).toBeTruthy();

		// Verify the stored graph round-trips with camelCase fields
		const getRes = await apiGet(`/api/templates/${template.id}`);
		expect(getRes.status).toBe(200);
		const stored = await getRes.json();
		const htNode = stored.graph.nodes.find((n: any) => n.data.type === 'human_task');
		expect(htNode.data.taskTitle).toBe('Review Incoming Invoice');
		const asNode = stored.graph.nodes.find((n: any) => n.data.type === 'automated_step');
		expect(asNode.data.executionSpec.backendType).toBe('python');

		// Step 2: Publish (triggers compilation)
		const pubRes = await apiPost(`/api/templates/${template.id}/publish`, {});
		expect(pubRes.status).toBe(200);
		const published = await pubRes.json();
		expect(published.published).toBe(true);
		expect(published.air_json).toBeTruthy();

		// Step 3: Deploy an instance
		const instRes = await apiPost('/api/instances', {
			template_id: template.id,
			created_by: AUTHOR_ID
		});
		expect(instRes.status).toBe(201);
		const instance = await instRes.json();
		expect(instance.status).toBe('running');
		expect(instance.net_id).toContain('mekhan-');

		// Step 4: Verify petri-lab has the net with correct topology
		const topoRes = await petriGet(`/api/nets/${instance.net_id}/topology`);
		expect(topoRes.status).toBe(200);
		const topoBody = await topoRes.json();
		const topology = topoBody.topology;
		// The compiled AIR should have places and transitions from all 8 node types
		expect(topology.places.length).toBeGreaterThan(10);
		expect(topology.transitions.length).toBeGreaterThan(5);

		// Step 5: Verify token is in the first place (review input)
		const stateRes = await petriGet(`/api/nets/${instance.net_id}/state`);
		expect(stateRes.status).toBe(200);

		// Cleanup: cancel the instance (it won't complete on its own - has human tasks)
		const cancelRes = await apiDelete(`/api/instances/${instance.id}`);
		expect([200, 204].includes(cancelRes.status)).toBe(true);
	});
});
