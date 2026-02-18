/**
 * Executor lifecycle E2E integration test.
 *
 * Exercises the full system end-to-end:
 *   1. Template creation with a Start → AutomatedStep → End graph
 *   2. Publication (triggers compilation to AIR with executor lifecycle subnet)
 *   3. Instance deployment (deploys net to petri-lab)
 *   4. Automated execution via real aithericon-executor (NATS signals: accepted → running → completed)
 *   5. Completion assertion (terminal marking, DB status)
 *
 * Requires: NATS, Postgres, petri-lab (with executor feature), aithericon-executor, mekhan-service
 * Run `./tests/e2e/scripts/ensure-infra.sh` to start all services.
 */

import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
	apiGet,
	apiDelete,
	petriGet,
	servicesHealthy,
	createAndPublish,
	createInstance,
	waitForInstanceStatus
} from './helpers';

const __dirname = fileURLToPath(new URL('.', import.meta.url));

// ---------------------------------------------------------------------------
// Test setup
// ---------------------------------------------------------------------------

test.beforeAll(async () => {
	const healthy = await servicesHealthy();
	test.skip(!healthy, 'Full stack is not running — skipping executor lifecycle tests');
});

const createdTemplateIds: string[] = [];
const createdInstanceIds: string[] = [];

test.afterAll(async () => {
	for (const id of createdInstanceIds) {
		try {
			await apiDelete(`/api/instances/${id}`);
		} catch {
			// ignore
		}
	}
	for (const id of createdTemplateIds) {
		try {
			await apiDelete(`/api/templates/${id}`);
		} catch {
			// ignore
		}
	}
});

// ---------------------------------------------------------------------------
// Test: Full executor lifecycle
// ---------------------------------------------------------------------------

test.describe('Executor Lifecycle E2E', () => {
	test('Start → AutomatedStep → End completes via executor', async () => {
		test.setTimeout(90_000);

		// 1. Load the graph fixture
		const fixturePath = resolve(
			__dirname,
			'../../fixtures/graphs/executor-auto-complete.json'
		);
		const graph = JSON.parse(readFileSync(fixturePath, 'utf8'));

		// 2. Create template + publish (triggers compilation to AIR)
		const template = await createAndPublish(`e2e-executor-${Date.now()}`, graph);
		createdTemplateIds.push(template.id);
		expect(template.published).toBe(true);
		expect(template.air_json).toBeTruthy();

		// 3. Verify the AIR contains executor lifecycle places
		const airRes = await apiGet(`/api/templates/${template.id}/air`);
		expect(airRes.status).toBe(200);
		const air = await airRes.json();

		expect(air.places).toBeDefined();
		expect(air.transitions).toBeDefined();

		// The executor lifecycle subnet should create signal places
		const placeIds = air.places.map((p: { id: string }) => p.id);
		const placeNames = air.places.map((p: { name: string }) => p.name);

		// Verify executor-specific places exist (from the SDK executor_lifecycle component)
		// The exact IDs are scoped like exec/sig_completed, exec/submitted, etc.
		const hasExecutorPlaces = placeNames.some((n: string) => n.toLowerCase().includes('completed'));
		expect(hasExecutorPlaces).toBe(true);

		// Verify there are more places than a simple Start→End (executor lifecycle adds ~20)
		expect(air.places.length).toBeGreaterThan(5);
		expect(air.transitions.length).toBeGreaterThan(3);

		// 4. Create instance (deploys net to petri-lab, starts eval loop)
		const instance = await createInstance(template.id);
		createdInstanceIds.push(instance.id);
		expect(instance.status).toBe('running');
		expect(instance.net_id).toMatch(/^mekhan-/);

		// 5. Verify the net was deployed to petri-lab
		const topoRes = await petriGet(`/api/nets/${instance.net_id}/topology`);
		expect(topoRes.status).toBe(200);
		const topoBody = await topoRes.json();
		expect(topoBody.topology.places.length).toBeGreaterThan(5);

		// 6. Verify initial state: token should be in the start/seeded place
		const stateRes = await petriGet(`/api/nets/${instance.net_id}/state`);
		expect(stateRes.status).toBe(200);

		// 7. Wait for the instance to complete via the real executor.
		//
		// The full lifecycle:
		//   start token → prepare transition (creates executor job payload)
		//   → submit effect (publishes job to NATS JetStream)
		//   → aithericon-executor picks up job, executes command, publishes: accepted → running → completed
		//   → executor watcher routes signals to petri-lab signal places
		//   → correlation transitions fire (sig_accepted + submitted → accepted, etc.)
		//   → token reaches exec/completed place
		//   → bridge transition moves token to p_exec_output
		//   → edge transition moves token to terminal place
		//   → petri-lab detects quiescence + terminal token → emits NetCompleted
		//   → lifecycle listener in mekhan-service updates DB status to 'completed'
		//
		// Timeout: 60s to account for cargo compilation if binary wasn't pre-built
		await waitForInstanceStatus(instance.id, ['completed'], 60_000);

		// 8. Verify final marking in petri-lab
		const finalStateRes = await petriGet(`/api/nets/${instance.net_id}/state`);
		if (finalStateRes.status === 200) {
			const finalState = await finalStateRes.json();
			const marking = finalState.marking;

			if (marking?.tokens) {
				// Find terminal places from the AIR
				const terminalPlaceIds = air.places
					.filter((p: { type: string }) => p.type === 'terminal')
					.map((p: { id: string }) => p.id);

				// At least one terminal place should have a token
				const terminalWithTokens = terminalPlaceIds.filter(
					(pid: string) => (marking.tokens[pid]?.length ?? 0) > 0
				);
				expect(terminalWithTokens.length).toBeGreaterThanOrEqual(1);

				// No non-terminal, non-signal places should have tokens (clean completion)
				const signalPlaceIds = air.places
					.filter((p: { type: string }) => p.type === 'signal')
					.map((p: { id: string }) => p.id);
				const excludeIds = new Set([...terminalPlaceIds, ...signalPlaceIds]);

				const strayTokens = Object.entries(marking.tokens)
					.filter(
						([pid, tokens]) =>
							!excludeIds.has(pid) && Array.isArray(tokens) && tokens.length > 0
					);
				expect(strayTokens).toEqual([]);
			}
		}
		// Note: net may have been hibernated/removed after completion, 404 is acceptable

		// 9. Verify instance status in mekhan DB
		const instanceRes = await apiGet(`/api/instances/${instance.id}`);
		expect(instanceRes.status).toBe(200);
		const finalInstance = await instanceRes.json();
		expect(finalInstance.status).toBe('completed');
		expect(finalInstance.completed_at).toBeTruthy();
	});
});
