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
 * Create an HPI API token by inserting directly into the SQLite DB.
 * This avoids the session auth / admin endpoint complexity.
 * Must be called once in beforeAll before using hpiGet/hpiPost.
 */
export async function initHpiToken(): Promise<void> {
	const { execSync } = await import('child_process');
	const { createHash, randomBytes } = await import('crypto');

	// Generate token: htk_ + 64 hex chars
	const rawToken = `htk_${randomBytes(32).toString('hex')}`;
	const tokenHash = createHash('sha256').update(rawToken).digest('hex');
	const tokenPrefix = rawToken.slice(0, 12);
	const id = crypto.randomUUID();

	const dbPath =
		process.env.HPI_DB_PATH ?? '../../hpi/app-hpi/data/auth.db';

	// Pipe SQL via stdin to avoid shell escaping issues with JSON scopes
	const sql = `INSERT OR REPLACE INTO api_token (id, name, token_hash, token_prefix, scopes, org_id, created_by, created_at) VALUES ('${id}', 'playwright-e2e', '${tokenHash}', '${tokenPrefix}', '["tasks:create","tasks:write","tasks:read"]', 'default', 'test', datetime('now'));`;

	// Retry with backoff to handle SQLite "database is locked" from parallel workers
	for (let attempt = 0; attempt < 5; attempt++) {
		try {
			execSync(`sqlite3 "${dbPath}"`, { input: sql, timeout: 5000 });
			break;
		} catch (e) {
			if (attempt === 4) throw e;
			await new Promise((r) => setTimeout(r, 200 * (attempt + 1)));
		}
	}

	hpiToken = rawToken;
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
