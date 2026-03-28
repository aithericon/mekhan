/**
 * Shared helpers for full-stack E2E integration tests.
 *
 * These helpers call real APIs against the running infrastructure:
 *   - mekhan-service (http://localhost:3100)
 *   - petri-lab      (http://localhost:3030)
 *   - HPI            (http://localhost:5188)
 */

import { expect } from '@playwright/test';

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

export const BACKEND = 'http://localhost:3100';
export const PETRI = 'http://localhost:3030';
export const HPI = 'http://localhost:5188';

/** A well-known author id (arbitrary UUID) used for all test requests. */
export const AUTHOR_ID = '00000000-0000-0000-0000-000000000001';

/** Polling config */
export const POLL_INTERVAL_MS = 500;
export const POLL_TIMEOUT_MS = 30_000;

// ---------------------------------------------------------------------------
// HTTP Helpers
// ---------------------------------------------------------------------------

/** POST JSON to a backend API path. */
export async function apiPost(path: string, body: unknown): Promise<Response> {
	return fetch(`${BACKEND}${path}`, {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify(body)
	});
}

/** PUT JSON to a backend API path. */
export async function apiPut(path: string, body: unknown): Promise<Response> {
	return fetch(`${BACKEND}${path}`, {
		method: 'PUT',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify(body)
	});
}

/** GET a backend API path. */
export async function apiGet(path: string): Promise<Response> {
	return fetch(`${BACKEND}${path}`);
}

/** DELETE a backend API path. */
export async function apiDelete(path: string): Promise<Response> {
	return fetch(`${BACKEND}${path}`, { method: 'DELETE' });
}

/** GET from petri-lab directly. */
export async function petriGet(path: string): Promise<Response> {
	return fetch(`${PETRI}${path}`);
}

/** POST JSON to petri-lab directly. */
export async function petriPost(path: string, body: unknown): Promise<Response> {
	return fetch(`${PETRI}${path}`, {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify(body)
	});
}

// ---------------------------------------------------------------------------
// HPI HTTP Helpers (Bearer token auth)
// ---------------------------------------------------------------------------

let hpiToken: string;

/**
 * Authenticate with HPI and obtain a Bearer token for API calls.
 * Must be called once in beforeAll before using hpiGet/hpiPost.
 */
export async function initHpiToken(): Promise<void> {
	// 1. Sign in as admin to get session cookie
	const signIn = await fetch(`${HPI}/api/auth/sign-in/email`, {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify({ email: 'system@hpi.dev', password: 'test1234' })
	});
	if (!signIn.ok) {
		throw new Error(`HPI sign-in failed: ${signIn.status} ${await signIn.text()}`);
	}
	const cookie = signIn.headers.get('set-cookie') ?? '';

	// 2. Create API token via admin endpoint
	const tokenRes = await fetch(`${HPI}/api/v1/admin/tokens`, {
		method: 'POST',
		headers: { 'content-type': 'application/json', cookie },
		body: JSON.stringify({
			name: `e2e-test-${Date.now()}`,
			scopes: ['tasks:read', 'tasks:write', 'tasks:create']
		})
	});
	if (!tokenRes.ok) {
		throw new Error(`HPI token creation failed: ${tokenRes.status} ${await tokenRes.text()}`);
	}
	const { token } = await tokenRes.json();
	hpiToken = token;
}

/** GET from HPI with Bearer auth. */
export async function hpiGet(path: string): Promise<Response> {
	return fetch(`${HPI}${path}`, {
		headers: { authorization: `Bearer ${hpiToken}` }
	});
}

/** POST JSON to HPI with Bearer auth. */
export async function hpiPost(path: string, body: unknown): Promise<Response> {
	return fetch(`${HPI}${path}`, {
		method: 'POST',
		headers: {
			'content-type': 'application/json',
			authorization: `Bearer ${hpiToken}`
		},
		body: JSON.stringify(body)
	});
}

// ---------------------------------------------------------------------------
// Polling
// ---------------------------------------------------------------------------

/**
 * Poll a condition until it returns true, or throw after timeout.
 */
export async function pollUntil(
	fn: () => Promise<boolean>,
	description: string,
	interval = POLL_INTERVAL_MS,
	timeout = POLL_TIMEOUT_MS
): Promise<void> {
	const start = Date.now();
	while (Date.now() - start < timeout) {
		if (await fn()) return;
		await new Promise((r) => setTimeout(r, interval));
	}
	throw new Error(`Timed out waiting for: ${description} (${timeout}ms)`);
}

// ---------------------------------------------------------------------------
// Health Check
// ---------------------------------------------------------------------------

/** Check that all services are reachable. Returns true if healthy. */
export async function servicesHealthy(): Promise<boolean> {
	try {
		const [backend, petri, hpi] = await Promise.all([
			fetch(`${BACKEND}/api/templates?page=1&per_page=1`).then((r) => r.ok),
			fetch(`${PETRI}/api/nets`).then((r) => r.ok),
			fetch(`${HPI}`).then((r) => r.ok).catch(() => false)
		]);
		return backend && petri && hpi;
	} catch {
		return false;
	}
}

// ---------------------------------------------------------------------------
// High-Level Workflow Helpers
// ---------------------------------------------------------------------------

/**
 * Create a template with a given graph, publish it, return the published template JSON.
 */
export async function createAndPublish(name: string, graph?: unknown) {
	const createBody: Record<string, unknown> = { name, author_id: AUTHOR_ID };
	if (graph) createBody.graph = graph;

	const createRes = await apiPost('/api/templates', createBody);
	expect(createRes.status).toBe(201);
	const template = await createRes.json();

	const pubRes = await apiPost(`/api/templates/${template.id}/publish`, {});
	expect(pubRes.status).toBe(200);
	const published = await pubRes.json();
	return published;
}

/**
 * Create an instance from a published template and return the instance JSON.
 */
export async function createInstance(templateId: string) {
	const res = await apiPost('/api/instances', {
		template_id: templateId,
		created_by: AUTHOR_ID
	});
	expect(res.status).toBe(201);
	return res.json();
}

/**
 * Poll petri-lab state until a token appears at the given place.
 * Returns the token's color value (the data payload).
 */
export async function waitForTokenAtPlace(
	netId: string,
	placeId: string,
	timeout = POLL_TIMEOUT_MS
): Promise<Record<string, unknown>> {
	let token: Record<string, unknown> | undefined;
	await pollUntil(
		async () => {
			const res = await petriGet(`/api/nets/${netId}/state`);
			if (!res.ok) return false;
			const state = await res.json();
			const tokens = state.marking?.tokens?.[placeId];
			if (Array.isArray(tokens) && tokens.length > 0) {
				token = tokens[0]?.color?.value;
				return true;
			}
			return false;
		},
		`token at ${placeId} in net ${netId}`,
		POLL_INTERVAL_MS,
		timeout
	);
	return token!;
}

/**
 * Poll HPI until a task with the given ID exists.
 * Returns the HPI task JSON.
 */
export async function waitForHpiTask(
	taskId: string,
	timeout = POLL_TIMEOUT_MS
): Promise<Record<string, unknown>> {
	let task: Record<string, unknown> | undefined;
	await pollUntil(
		async () => {
			const res = await hpiGet(`/api/v1/tasks/${taskId}`);
			if (!res.ok) return false;
			task = await res.json();
			return true;
		},
		`HPI task ${taskId}`,
		POLL_INTERVAL_MS,
		timeout
	);
	return task!;
}

/**
 * Wait for an instance to reach one of the given statuses.
 */
export async function waitForInstanceStatus(
	instanceId: string,
	statuses: string[],
	timeout = POLL_TIMEOUT_MS
) {
	let lastStatus = '';
	await pollUntil(
		async () => {
			const res = await apiGet(`/api/instances/${instanceId}`);
			if (!res.ok) return false;
			const data = await res.json();
			lastStatus = data.status;
			return statuses.includes(data.status);
		},
		`instance ${instanceId} to reach status [${statuses.join(', ')}] (last: ${lastStatus})`,
		POLL_INTERVAL_MS,
		timeout
	);
}
