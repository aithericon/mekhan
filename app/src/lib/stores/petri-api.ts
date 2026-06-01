/**
 * Typed transport for the petri-lab engine (`/petri/api/nets/:id/...`).
 *
 * petri-lab is a separate engine and its routes are not in mekhan's OpenAPI
 * schema, so this is a hand-written typed client rather than an `openapi-fetch`
 * wrapper. The point of the extraction is to remove the raw `fetch` calls and
 * the three divergent error conventions that were scattered through the store:
 * every request here funnels through one helper and throws a single
 * `PetriApiError` on a non-2xx (carrying status + body). The store decides how
 * to surface that (set `error`, swallow for non-critical reads, or map to a
 * `{ success }` result) — that policy is orchestration, not transport.
 */

import type { PetriNet, PersistedEvent, ScenarioGroup, TokenColor } from '$lib/types/petri';

export class PetriApiError extends Error {
	constructor(
		readonly status: number,
		readonly body: string
	) {
		super(`Petri API error ${status}: ${body}`);
		this.name = 'PetriApiError';
	}
}

export interface TopologyResult {
	topology: PetriNet;
	groups: ScenarioGroup[];
}

export interface ScenarioLoadResult {
	places_count?: number;
	transitions_count?: number;
	tokens_count?: number;
}

async function request(
	url: string,
	init?: RequestInit
): Promise<Response> {
	const res = await fetch(url, init);
	if (!res.ok) {
		const body = await res.text().catch(() => '');
		throw new PetriApiError(res.status, body);
	}
	return res;
}

async function requestJson<T>(url: string, init?: RequestInit): Promise<T> {
	const res = await request(url, init);
	return res.json() as Promise<T>;
}

const JSON_HEADERS = { 'Content-Type': 'application/json' };

export function createPetriApi(apiBase: string) {
	async function fetchTopology(): Promise<TopologyResult> {
		const data = await requestJson<Record<string, unknown>>(`${apiBase}/topology`);
		// Engine returns TopologyResponse: { topology: { places, transitions, arcs, groups } }
		const net = (data.topology ?? data.net ?? data) as PetriNet & {
			groups?: ScenarioGroup[];
		};
		const groups = (net?.groups ??
			(data.groups as ScenarioGroup[] | undefined) ??
			[]) as ScenarioGroup[];
		return { topology: net, groups };
	}

	async function fetchEvents(fromSequence?: number): Promise<PersistedEvent[]> {
		const suffix =
			fromSequence !== undefined ? `?from_sequence=${fromSequence}` : '';
		const data = await requestJson<{ events?: PersistedEvent[] }>(
			`${apiBase}/events${suffix}`
		);
		return data.events ?? [];
	}

	async function fetchState(): Promise<Record<string, string> | null> {
		const data = await requestJson<{
			transition_statuses?: Record<string, string>;
		}>(`${apiBase}/state`);
		return data.transition_statuses ?? null;
	}

	async function fetchRunMode(): Promise<string> {
		const data = await requestJson<{ mode?: string }>(`${apiBase}/run-mode`);
		return data.mode ?? 'stopped';
	}

	async function fetchAnalysis<T>(): Promise<T> {
		return requestJson<T>(`${apiBase}/analysis`);
	}

	async function fetchServices<T>(): Promise<T> {
		return requestJson<T>(`${apiBase}/services`);
	}

	async function fireTransition(transitionId: string): Promise<void> {
		await request(`${apiBase}/command/fire/${transitionId}`, { method: 'POST' });
	}

	async function createToken(placeId: string, color: TokenColor): Promise<void> {
		await request(`${apiBase}/command/create-token`, {
			method: 'POST',
			headers: JSON_HEADERS,
			body: JSON.stringify({ place_id: placeId, color })
		});
	}

	async function evaluate(maxSteps: number): Promise<void> {
		await request(`${apiBase}/command/evaluate`, {
			method: 'POST',
			headers: JSON_HEADERS,
			body: JSON.stringify({ max_steps: maxSteps })
		});
	}

	async function reset(): Promise<void> {
		await request(`${apiBase}/command/reset`, { method: 'POST' });
	}

	async function setRunMode(mode: string): Promise<void> {
		await request(`${apiBase}/run-mode`, {
			method: 'PUT',
			headers: JSON_HEADERS,
			body: JSON.stringify({ mode })
		});
	}

	async function hibernate(): Promise<void> {
		await request(`${apiBase}/command/hibernate`, { method: 'POST' });
	}

	async function loadScenario(scenario: unknown): Promise<ScenarioLoadResult> {
		// Wire shape: LoadScenarioRequest envelope `{ scenario, skip_mask?, stage_overrides? }`
		// (sub-phase 2.5e-γ.mekhan-S3 cutover; the bare-scenario request shape was
		// retired with the scaffold envelope cutover on the engine side per
		// `feedback_no_backward_compat_hedging_in_migration_waves` +
		// `feedback_delete_superseded_code`). The frontend editor does not drive
		// ablation; `skip_mask`/`stage_overrides` are omitted (engine deserialises
		// them as empty via serde defaults).
		return requestJson<ScenarioLoadResult>(`${apiBase}/scenario`, {
			method: 'POST',
			headers: JSON_HEADERS,
			body: JSON.stringify({ scenario })
		});
	}

	async function saveTransitionScript(
		transitionId: string,
		script: string,
		guard: string | null
	): Promise<void> {
		await request(`${apiBase}/topology/transition/${transitionId}`, {
			method: 'PATCH',
			headers: JSON_HEADERS,
			body: JSON.stringify({ script, guard })
		});
	}

	return {
		fetchTopology,
		fetchEvents,
		fetchState,
		fetchRunMode,
		fetchAnalysis,
		fetchServices,
		fireTransition,
		createToken,
		evaluate,
		reset,
		setRunMode,
		hibernate,
		loadScenario,
		saveTransitionScript
	};
}

export type PetriApi = ReturnType<typeof createPetriApi>;
