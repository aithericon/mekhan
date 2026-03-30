/**
 * E2E tests for the mekhan task proxy endpoints.
 *
 * Tests the new /api/tasks/* routes that proxy to HPI,
 * and the /api/tasks/stream SSE endpoint.
 *
 * Requires full stack: mekhan-service, petri-lab, HPI, NATS.
 * mekhan-service must be configured with HPI URL and API token.
 */

import { test, expect } from '@playwright/test';
import {
	apiPost,
	apiGet,
	apiDelete,
	servicesHealthy,
	initHpiToken,
	createAndPublish,
	createInstance,
	waitForTokenAtPlace,
	pollUntil,
	waitForInstanceStatus,
	BACKEND
} from './helpers';

// Run serially — HPI SQLite doesn't handle concurrent writes well
test.describe.configure({ mode: 'serial' });

test.beforeAll(async () => {
	const healthy = await servicesHealthy();
	test.skip(!healthy, 'Full stack is not running — skipping task proxy tests');
	await initHpiToken();
});

// ---------------------------------------------------------------------------
// Shared graph
// ---------------------------------------------------------------------------

const humanTaskGraph = {
	nodes: [
		{
			id: 'start',
			type: 'start',
			position: { x: 250, y: 100 },
			data: { type: 'start', label: 'Start', description: null, initialData: null }
		},
		{
			id: 'ht1',
			type: 'human_task',
			position: { x: 250, y: 250 },
			data: {
				type: 'human_task',
				label: 'Review',
				description: null,
				taskTitle: 'Task Proxy Test',
				instructionsMdsvex: 'Please review this item.',
				steps: [
					{
						id: 'step1',
						title: 'Review',
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
							},
							{
								type: 'input',
								field: {
									name: 'notes',
									label: 'Notes',
									kind: 'textarea'
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
			data: { type: 'end', label: 'End', description: null }
		}
	],
	edges: [
		{ id: 'e1', source: 'start', target: 'ht1', source_handle: null, label: null, type: 'sequence' },
		{ id: 'e2', source: 'ht1', target: 'end', source_handle: null, label: null, type: 'sequence' }
	]
};

/** Deploy a workflow and wait for the human task to appear in HPI */
async function deployAndWaitForTask() {
	const template = await createAndPublish(`e2e-proxy-${Date.now()}`, humanTaskGraph);
	const instance = await createInstance(template.id);

	// Wait for engine to fire the human task effect
	const activeToken = await waitForTokenAtPlace(instance.net_id, 'p_ht1_active', 30_000);
	const taskId = String(activeToken.task_id);
	expect(taskId).toBeTruthy();

	// Wait for HPI to consume from NATS — poll via mekhan proxy
	await pollUntil(
		async () => {
			const res = await apiGet(`/api/tasks/${taskId}`);
			return res.ok;
		},
		`task ${taskId} via mekhan proxy`,
		45_000,
		500
	);

	return { template, instance, taskId };
}

// ---------------------------------------------------------------------------
// Test: List tasks via proxy
// ---------------------------------------------------------------------------

test('GET /api/tasks returns task list from HPI', async () => {
	test.setTimeout(60_000);

	const { instance, taskId } = await deployAndWaitForTask();

	// List tasks via mekhan proxy
	const listRes = await apiGet('/api/tasks?status=pending');
	expect(listRes.status).toBe(200);
	const listBody = await listRes.json();
	expect(listBody.tasks).toBeDefined();
	expect(Array.isArray(listBody.tasks)).toBe(true);

	// Our task should be in the list
	const ourTask = listBody.tasks.find(
		(t: any) => (t.task_id ?? t.id) === taskId
	);
	expect(ourTask).toBeTruthy();
	expect(ourTask.title).toBe('Task Proxy Test');
	expect(ourTask.status).toBe('pending');

	// Get single task via proxy
	const getRes = await apiGet(`/api/tasks/${taskId}`);
	expect(getRes.status).toBe(200);
	const taskDetail = await getRes.json();
	expect(taskDetail.task_id ?? taskDetail.id).toBe(taskId);
	expect(taskDetail.title).toBe('Task Proxy Test');
	expect(taskDetail.status).toBe('pending');

	await apiDelete(`/api/instances/${instance.id}`);
});

// ---------------------------------------------------------------------------
// Test: Complete task via proxy
// ---------------------------------------------------------------------------

test('POST /api/tasks/:id/complete completes via HPI and finishes workflow', async () => {
	test.setTimeout(60_000);

	const { instance, taskId } = await deployAndWaitForTask();

	// Complete via mekhan proxy
	const completeRes = await apiPost(`/api/tasks/${taskId}/complete`, {
		data: { approved: true, notes: 'Looks good' }
	});
	expect(completeRes.status).toBe(200);

	// Verify completed
	const getRes = await apiGet(`/api/tasks/${taskId}`);
	expect(getRes.status).toBe(200);
	const completedTask = await getRes.json();
	expect(completedTask.status).toBe('completed');

	// Workflow should complete (signal flows back through NATS)
	await waitForInstanceStatus(instance.id, ['completed'], 30_000);
});

// ---------------------------------------------------------------------------
// Test: Cancel task via proxy
// ---------------------------------------------------------------------------

test('POST /api/tasks/:id/cancel cancels the task', async () => {
	test.setTimeout(60_000);

	const { instance, taskId } = await deployAndWaitForTask();

	const cancelRes = await apiPost(`/api/tasks/${taskId}/cancel`, {
		reason: 'Changed my mind'
	});
	expect(cancelRes.status).toBe(200);

	const getRes = await apiGet(`/api/tasks/${taskId}`);
	expect(getRes.status).toBe(200);
	const cancelledTask = await getRes.json();
	expect(cancelledTask.status).toBe('cancelled');

	await apiDelete(`/api/instances/${instance.id}`);
});

// ---------------------------------------------------------------------------
// Test: SSE task stream
// ---------------------------------------------------------------------------

test('GET /api/tasks/stream connects and receives events', async () => {
	test.setTimeout(60_000);

	// Connect to SSE
	const controller = new AbortController();
	const resp = await fetch(`${BACKEND}/api/tasks/stream`, {
		signal: controller.signal
	});
	expect(resp.status).toBe(200);
	expect(resp.headers.get('content-type')).toContain('text/event-stream');

	const reader = resp.body!.getReader();
	const decoder = new TextDecoder();

	// Read first chunk — should be "connected" event
	const { value } = await reader.read();
	const text = decoder.decode(value);
	expect(text).toContain('event: connected');

	// Deploy a workflow to trigger a task_created event
	const template = await createAndPublish(`e2e-sse-${Date.now()}`, humanTaskGraph);
	const instance = await createInstance(template.id);

	// Read SSE events looking for task_created
	let gotTaskCreated = false;
	const deadline = Date.now() + 15_000;
	while (Date.now() < deadline && !gotTaskCreated) {
		const readResult = await Promise.race([
			reader.read(),
			new Promise<{ done: true; value: undefined }>((resolve) =>
				setTimeout(() => resolve({ done: true, value: undefined }), 2000)
			)
		]);
		if (readResult.done || !readResult.value) continue;
		const chunk = decoder.decode(readResult.value);
		if (chunk.includes('event: task_created')) {
			gotTaskCreated = true;
		}
	}

	controller.abort();
	expect(gotTaskCreated).toBe(true);

	await apiDelete(`/api/instances/${instance.id}`);
});
