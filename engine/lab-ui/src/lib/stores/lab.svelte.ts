import { api, type PetriNet, type PersistedEvent, type Marking, type Token, type TokenColor, type TransitionStatus } from '$lib/api/client';

// Analysis types
export type IssueLevel = 'error' | 'warning' | 'info';

export interface ValidationIssue {
	node_id: string;
	node_type: string;
	level: IssueLevel;
	code: string;
	message: string;
	remote_net_id?: string;
}

export interface AnalysisSummary {
	error_count: number;
	warning_count: number;
	info_count: number;
}

export interface AnalysisReport {
	is_valid: boolean;
	issues: ValidationIssue[];
	summary: AnalysisSummary;
}

export interface ServicesResponse {
	handlers: string[];
	categories: Record<string, string[]>;
}

// Selection types for Inspector
export type SelectedElement =
	| { type: 'place'; id: string }
	| { type: 'transition'; id: string }
	| { type: 'token'; placeId: string; tokenId: string }
	| { type: 'event'; sequence: number }
	| { type: 'group'; id: string }
	| { type: 'remotenet'; id: string; label: string; targets: string[]; sources: string[]; childNetIds: string[] }
	| null;

// Run mode type
export type RunMode = 'stopped' | 'running';

// Event spotlight — computed from selected event for canvas highlighting
export interface EventSpotlight {
	transitionId: string | null;
	consumedPlaceIds: string[];
	producedPlaceIds: string[];
	targetPlaceId: string | null; // TokenCreated target
	allNodeIds: string[]; // union for fitView
}

// Marking diff — brief pulse when stepping through timeline
export interface MarkingDiff {
	appeared: string[]; // place IDs where tokens appeared
	disappeared: string[]; // place IDs where tokens disappeared
	firedTransition: string | null;
}

// Result type for loadScenario
interface LoadScenarioResult {
	success: boolean;
	places_count?: number;
	transitions_count?: number;
	tokens_count?: number;
	error?: string;
}

// Scenario group type
export interface ScenarioGroup {
	id: string;
	name: string;
	parent_id?: string;
	metadata?: Record<string, unknown>;
}

/** The shape of a lab store instance returned by createLabStore. */
export type LabStore = ReturnType<typeof createLabStore>;

/**
 * Factory function that creates an isolated lab store for a given net.
 *
 * @param netId - The net identifier. All API calls use `/api/nets/{netId}/*` routes.
 */
export function createLabStore(netId: string) {
	const apiBase = `/api/nets/${netId}`;

	// Reactive state using Svelte 5 runes
	let topology = $state<PetriNet | null>(null);
	let events = $state<PersistedEvent[]>([]);
	let replayIndex = $state(-1);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let selectedElement = $state<SelectedElement>(null);
	let transitionStatuses = $state<Record<string, TransitionStatus>>({});

	// Track last fetched sequence for incremental polling
	let lastFetchedSequence = $state<number>(0);

	// Run mode state (backend-controlled)
	let runMode = $state<RunMode>('stopped');
	let evaluating = $state(false); // Lock for one-shot evaluate
	let pollingIntervalId = $state<ReturnType<typeof setInterval> | null>(null);
	const POLLING_INTERVAL_MS = 500; // Fallback poll interval (only used when SSE unavailable)

	// SSE (Server-Sent Events) state
	let sseAbortController = $state<AbortController | null>(null);
	let sseConnected = $state(false);
	let sseRetryCount = 0;
	const SSE_MAX_RETRIES = 5;
	const SSE_INITIAL_RETRY_MS = 1000;

	// Current scenario (for adapter manager)
	let currentScenario = $state<unknown>(null);

	// Groups from current scenario (for visualization)
	let currentGroups = $state<ScenarioGroup[]>([]);

	// Static analysis state
	let analysisReport = $state<AnalysisReport | null>(null);

	// Services state
	let services = $state<ServicesResponse | null>(null);

	// Derived lookup maps for ID -> Name resolution
	const transitionNameMap = $derived.by(() => {
		const map = new Map<string, string>();
		if (topology) {
			for (const t of topology.transitions) {
				map.set(t.id, t.name);
			}
		}
		return map;
	});

	const placeNameMap = $derived.by(() => {
		const map = new Map<string, string>();
		if (topology) {
			for (const p of topology.places) {
				map.set(p.id, p.name);
			}
		}
		return map;
	});

	// Helper functions to resolve IDs to names
	function getTransitionName(id: string): string {
		return transitionNameMap.get(id) ?? id;
	}

	function getPlaceName(id: string): string {
		return placeNameMap.get(id) ?? id;
	}

	// Resolve UUIDs in error messages to human-readable names (only TokenId is still UUID)
	function resolveErrorMessage(msg: string): string {
		const uuidPattern = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi;
		return msg.replace(uuidPattern, (uuid) => {
			const transitionName = transitionNameMap.get(uuid);
			if (transitionName) return `"${transitionName}"`;
			const placeName = placeNameMap.get(uuid);
			if (placeName) return `"${placeName}"`;
			return uuid;
		});
	}

	// Derived state: compute marking at replay index
	const projectedMarking = $derived.by(() => {
		const marking: Map<string, Token[]> = new Map();

		if (replayIndex < 0) return marking;
		const eventsToReplay = events.slice(0, replayIndex + 1);

		for (const persisted of eventsToReplay) {
			const event = persisted.event;
			const eventType: string = event.type;

			if (eventType === 'TokenCreated') {
				const placeId = (event as any).place_id;
				const token = (event as any).token;
				if (!marking.has(placeId)) {
					marking.set(placeId, []);
				}
				marking.get(placeId)!.push(token);
			} else if (eventType === 'TransitionFired' || eventType === 'EffectCompleted' || eventType === 'EffectFailed') {
				const consumed = (event as any).consumed_tokens as [string, string][];
				const produced = (event as any).produced_tokens as [string, Token][];

				// Remove consumed tokens
				if (consumed) {
					for (const [placeId, tokenId] of consumed) {
						const tokens = marking.get(placeId);
						if (tokens) {
							const idx = tokens.findIndex((t) => t.id === tokenId);
							if (idx >= 0) tokens.splice(idx, 1);
						}
					}
				}

				// Add produced tokens
				if (produced) {
					for (const [placeId, token] of produced) {
						if (!marking.has(placeId)) {
							marking.set(placeId, []);
						}
						marking.get(placeId)!.push(token);
					}
				}
			} else if (eventType === 'TokenConsumed' || eventType === 'TokenRemoved') {
				const placeId = (event as any).place_id;
				const tokenId = (event as any).token_id;
				const tokens = marking.get(placeId);
				if (tokens) {
					const idx = tokens.findIndex((t) => t.id === tokenId);
					if (idx >= 0) tokens.splice(idx, 1);
				}
			}
		}

		return marking;
	});

	// Derived state: accumulate tokens sent through bridge_out places
	const bridgedOutTokens = $derived.by(() => {
		const bridged: Map<string, Token[]> = new Map();
		if (replayIndex < 0) return bridged;
		const eventsToReplay = events.slice(0, replayIndex + 1);
		for (const persisted of eventsToReplay) {
			const event = persisted.event;
			if (event.type === 'TokenBridgedOut') {
				const placeId = (event as any).source_place_id;
				const token = (event as any).token;
				if (!bridged.has(placeId)) bridged.set(placeId, []);
				bridged.get(placeId)!.push(token);
			}
		}
		return bridged;
	});

	// Derived: compute spotlight from selected event for canvas highlighting
	const eventSpotlight = $derived.by((): EventSpotlight | null => {
		const elem = selectedElement;
		if (!elem || elem.type !== 'event') return null;
		const event = events.find(e => e.sequence === elem.sequence);
		if (!event) return null;

		const e = event.event as any;
		const type = e.type as string;

		let transitionId: string | null = null;
		let consumedPlaceIds: string[] = [];
		let producedPlaceIds: string[] = [];
		let targetPlaceId: string | null = null;

		switch (type) {
			case 'TransitionFired':
			case 'EffectCompleted':
			case 'EffectFailed':
				transitionId = e.transition_id;
				consumedPlaceIds = (e.consumed_tokens as [string, string][] ?? []).map(([pid]) => pid);
				producedPlaceIds = (e.produced_tokens as [string, any][] ?? []).map(([pid]) => pid);
				break;
			case 'TokenCreated':
				targetPlaceId = e.place_id;
				break;
			case 'TokenConsumed':
			case 'TokenRemoved':
				consumedPlaceIds = [e.place_id];
				break;
			case 'TokenBridgedOut':
				transitionId = e.transition_id;
				consumedPlaceIds = [e.source_place_id];
				break;
		}

		const allSet = new Set<string>();
		if (transitionId) allSet.add(transitionId);
		consumedPlaceIds.forEach(id => allSet.add(id));
		producedPlaceIds.forEach(id => allSet.add(id));
		if (targetPlaceId) allSet.add(targetPlaceId);

		if (allSet.size === 0) return null;

		return {
			transitionId,
			consumedPlaceIds: [...new Set(consumedPlaceIds)],
			producedPlaceIds: [...new Set(producedPlaceIds)],
			targetPlaceId,
			allNodeIds: [...allSet]
		};
	});

	// Marking diff pulse — tracks what changed on single timeline steps
	let markingDiff = $state<MarkingDiff | null>(null);
	let diffTimeoutId: ReturnType<typeof setTimeout> | null = null;

	/** Compute marking diff from an event for pulse animation */
	function computeDiffFromEvent(ev: any, forward: boolean): MarkingDiff {
		const diff: MarkingDiff = { appeared: [], disappeared: [], firedTransition: null };
		const type = ev.type as string;
		switch (type) {
			case 'TransitionFired':
			case 'EffectCompleted':
			case 'EffectFailed':
				diff.firedTransition = ev.transition_id;
				if (forward) {
					diff.disappeared = (ev.consumed_tokens as [string, string][] ?? []).map(([pid]) => pid);
					diff.appeared = (ev.produced_tokens as [string, any][] ?? []).map(([pid]) => pid);
				} else {
					diff.appeared = (ev.consumed_tokens as [string, string][] ?? []).map(([pid]) => pid);
					diff.disappeared = (ev.produced_tokens as [string, any][] ?? []).map(([pid]) => pid);
				}
				break;
			case 'TokenCreated':
				if (forward) diff.appeared = [ev.place_id];
				else diff.disappeared = [ev.place_id];
				break;
			case 'TokenConsumed':
			case 'TokenRemoved':
				if (forward) diff.disappeared = [ev.place_id];
				else diff.appeared = [ev.place_id];
				break;
			case 'TokenBridgedOut':
				diff.firedTransition = ev.transition_id;
				if (forward) diff.disappeared = [ev.source_place_id];
				else diff.appeared = [ev.source_place_id];
				break;
		}
		return diff;
	}

	// API functions — use apiBase for all fetch calls
	async function fetchTopology() {
		loading = true;
		error = null;
		try {
			const response = await fetch(`${apiBase}/topology`);
			const data = await response.json();
			topology = data?.topology ?? null;
			// Groups are part of the topology (persisted in NetInitialized event)
			currentGroups = (data?.topology?.groups as ScenarioGroup[] | undefined) ?? [];
		} catch (e) {
			error = e instanceof Error ? e.message : 'Unknown error';
		} finally {
			loading = false;
		}
	}

	async function fetchEvents() {
		loading = true;
		error = null;
		try {
			const response = await fetch(`${apiBase}/events`);
			const data = await response.json();
			events = data?.events ?? [];
			// Update last fetched sequence
			if (events.length > 0) {
				lastFetchedSequence = events[events.length - 1].sequence;
			} else {
				lastFetchedSequence = 0;
			}
			// Set replay index to the end
			if (events.length > 0 && replayIndex < 0) {
				replayIndex = events.length - 1;
			}
			// Also fetch state to get transition statuses
			await fetchState();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Unknown error';
		} finally {
			loading = false;
		}
	}

	// Lock to prevent concurrent fetchNewEvents calls
	let fetchingNewEvents = false;

	/** Fetch only new events since lastFetchedSequence (for efficient polling) */
	async function fetchNewEvents() {
		// Prevent concurrent fetches which could cause duplicate events
		if (fetchingNewEvents) return;
		fetchingNewEvents = true;

		try {
			// Fetch events starting from the next sequence after our last
			const fromSeq = lastFetchedSequence + 1;
			const response = await fetch(`${apiBase}/events?from_sequence=${fromSeq}`);
			const data = await response.json();
			const newEvents: PersistedEvent[] = data?.events ?? [];

			if (newEvents.length > 0) {
				// Filter out any events we already have (safety deduplication)
				const existingSeqs = new Set(events.map(e => e.sequence));
				const uniqueNewEvents = newEvents.filter(e => !existingSeqs.has(e.sequence));

				if (uniqueNewEvents.length > 0) {
					// Append unique new events
					events = [...events, ...uniqueNewEvents];
					lastFetchedSequence = uniqueNewEvents[uniqueNewEvents.length - 1].sequence;
					// Move replay index to end
					replayIndex = events.length - 1;
					// Fetch state for updated transition statuses
					await fetchState();
				}
			}
		} catch (e) {
			console.error('Failed to fetch new events:', e);
		} finally {
			fetchingNewEvents = false;
		}
	}

	async function fetchState() {
		try {
			const response = await fetch(`${apiBase}/state`);
			const data = await response.json();
			if (data.transition_statuses) {
				transitionStatuses = data.transition_statuses;
			}
		} catch (e) {
			console.error('Failed to fetch state:', e);
		}
	}

	async function fetchAnalysis() {
		try {
			const response = await fetch(`${apiBase}/analyze`);
			const data: AnalysisReport = await response.json();
			analysisReport = data;
		} catch (e) {
			console.error('Failed to fetch analysis:', e);
		}
	}

	async function fetchServices() {
		try {
			const response = await fetch(`${apiBase}/services`);
			const data: ServicesResponse = await response.json();
			services = data;
		} catch (e) {
			console.error('Failed to fetch services:', e);
		}
	}

	async function fireTransition(transitionId: string) {
		loading = true;
		error = null;
		try {
			const response = await fetch(`${apiBase}/command/fire/${transitionId}`, {
				method: 'POST'
			});
			const data = await response.json();
			if (!data?.success) {
				const errorMsg = data?.error ?? 'Failed to fire transition';
				throw new Error(errorMsg);
			}
			// Refresh events after firing
			await fetchEvents();
			// Move to the end
			replayIndex = events.length - 1;
		} catch (e) {
			const rawMsg = e instanceof Error ? e.message : 'Unknown error';
			error = resolveErrorMessage(rawMsg);
		} finally {
			loading = false;
		}
	}

	async function reset() {
		loading = true;
		error = null;
		try {
			await fetch(`${apiBase}/command/reset`, { method: 'POST' });
			events = [];
			replayIndex = -1;
			lastFetchedSequence = 0;
			// Re-fetch to get initial state
			await fetchTopology();
			await fetchEvents();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Unknown error';
		} finally {
			loading = false;
		}
	}

	async function loadScenario(scenario: unknown): Promise<LoadScenarioResult> {
		loading = true;
		error = null;
		try {
			const response = await fetch(`${apiBase}/scenario`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify(scenario)
			});
			const data = await response.json();
			if (data.success) {
				// Store scenario reference
				currentScenario = scenario;
				// Extract groups from scenario for visualization
				const scenarioObj = scenario as { groups?: ScenarioGroup[] };
				currentGroups = scenarioObj.groups ?? [];
				// Reset replay index and sequence tracking
				events = [];
				replayIndex = -1;
				lastFetchedSequence = 0;
				return {
					success: true,
					places_count: data.places_count,
					transitions_count: data.transitions_count,
					tokens_count: data.tokens_count
				};
			} else {
				return { success: false, error: data.error ?? 'Failed to load scenario' };
			}
		} catch (e) {
			const msg = e instanceof Error ? e.message : 'Unknown error';
			error = msg;
			return { success: false, error: msg };
		} finally {
			loading = false;
		}
	}

	function setReplayIndex(index: number) {
		const newIndex = Math.max(-1, Math.min(index, events.length - 1));
		const step = newIndex - replayIndex;

		// Compute pulse diff for single-step changes only
		if (Math.abs(step) === 1 && newIndex >= 0 && newIndex < events.length) {
			if (step === 1) {
				markingDiff = computeDiffFromEvent(events[newIndex].event, true);
			} else {
				markingDiff = computeDiffFromEvent(events[newIndex + 1].event, false);
			}
			if (diffTimeoutId) clearTimeout(diffTimeoutId);
			diffTimeoutId = setTimeout(() => { markingDiff = null; }, 600);
		} else if (step !== 0) {
			markingDiff = null;
		}

		replayIndex = newIndex;
	}

	// Selection functions for Inspector
	function selectPlace(id: string) {
		selectedElement = { type: 'place', id };
	}

	function selectTransition(id: string) {
		selectedElement = { type: 'transition', id };
	}

	function selectToken(placeId: string, tokenId: string) {
		selectedElement = { type: 'token', placeId, tokenId };
	}

	function selectEvent(sequence: number) {
		selectedElement = { type: 'event', sequence };
	}

	function selectGroup(id: string) {
		selectedElement = { type: 'group', id };
	}

	function selectRemoteNet(id: string, label: string, targets: string[], sources: string[], childNetIds: string[]) {
		selectedElement = { type: 'remotenet', id, label, targets, sources, childNetIds };
	}

	function clearSelection() {
		selectedElement = null;
	}

	// Token injection - create a new token at a place with specific data
	async function injectToken(placeId: string, data: unknown): Promise<{ success: boolean; error?: string }> {
		loading = true;
		error = null;
		try {
			const color: TokenColor = data === null ? { type: 'Unit' } : { type: 'Data', value: data };
			const response = await fetch(`${apiBase}/command/create-token`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ place_id: placeId, color })
			});
			const responseData = await response.json();
			if (!responseData?.success) {
				const errorMsg = responseData?.error ?? 'Failed to inject token';
				throw new Error(errorMsg);
			}
			// Refresh events after injection
			await fetchEvents();
			replayIndex = events.length - 1;
			return { success: true };
		} catch (e) {
			const rawMsg = e instanceof Error ? e.message : 'Unknown error';
			const resolvedMsg = resolveErrorMessage(rawMsg);
			error = resolvedMsg;
			return { success: false, error: resolvedMsg };
		} finally {
			loading = false;
		}
	}

	// Run mode functions (backend-controlled execution)

	// ── SSE connection ─────────────────────────────────────────────────

	/**
	 * Open an SSE connection using fetch + ReadableStream.
	 * Unlike EventSource, this gives us full control over reconnection
	 * and doesn't auto-reconnect when the server dies.
	 */
	function connectSSE() {
		if (sseAbortController) return; // Already connected

		const controller = new AbortController();
		sseAbortController = controller;

		// In dev mode, connect directly to the backend to bypass Vite proxy
		const sseBase = import.meta.env.DEV ? `http://localhost:3030${apiBase}` : apiBase;
		const fromSeq = lastFetchedSequence > 0 ? lastFetchedSequence + 1 : undefined;
		const url = fromSeq != null
			? `${sseBase}/events/stream?from_sequence=${fromSeq}`
			: `${sseBase}/events/stream`;

		(async () => {
			try {
				const response = await fetch(url, {
					signal: controller.signal,
					headers: { Accept: 'text/event-stream' }
				});

				if (!response.ok || !response.body) {
					throw new Error(`SSE response ${response.status}`);
				}

				sseConnected = true;
				sseRetryCount = 0;
				stopPolling();

				const reader = response.body.getReader();
				const decoder = new TextDecoder();
				let buffer = '';

				while (true) {
					const { done, value } = await reader.read();
					if (done) break;

					buffer += decoder.decode(value, { stream: true });

					// Parse SSE frames: split on double newline
					const frames = buffer.split('\n\n');
					buffer = frames.pop()!; // Keep incomplete frame in buffer

					for (const frame of frames) {
						if (!frame.trim()) continue;
						const lines = frame.split('\n');
						let eventType = '';
						let data = '';
						for (const line of lines) {
							if (line.startsWith('event:')) eventType = line.slice(6).trim();
							else if (line.startsWith('data:')) data = line.slice(5).trim();
							// Ignore comment lines (keepalive pings)
						}

						if (eventType === 'update' && data) {
							try {
								const event: PersistedEvent = JSON.parse(data);
								if (event.sequence <= lastFetchedSequence) continue;
								events = [...events, event];
								lastFetchedSequence = event.sequence;
								replayIndex = events.length - 1;
								fetchState();
							} catch {
								// ignore parse errors
							}
						} else if (eventType === 'reset') {
							events = [];
							replayIndex = -1;
							lastFetchedSequence = 0;
							fetchTopology();
							fetchEvents();
						} else if (eventType === 'resync') {
							fetchNewEvents();
						}
					}
				}
			} catch (e) {
				// AbortError is expected on disconnect — don't retry
				if (e instanceof DOMException && e.name === 'AbortError') return;
			}

			// Stream ended or errored — clean up and retry
			sseConnected = false;
			sseAbortController = null;

			if (sseRetryCount < SSE_MAX_RETRIES) {
				sseRetryCount++;
				const delay = SSE_INITIAL_RETRY_MS * Math.pow(2, sseRetryCount - 1);
				console.log(`[Lab:${netId}] SSE disconnected, retry ${sseRetryCount}/${SSE_MAX_RETRIES} in ${delay / 1000}s`);
				setTimeout(() => connectSSE(), delay);
			} else {
				console.log(`[Lab:${netId}] SSE gave up after ${SSE_MAX_RETRIES} retries`);
			}
		})();
	}

	/** Close the SSE connection. */
	function disconnectSSE() {
		if (sseAbortController) {
			sseAbortController.abort();
			sseAbortController = null;
			sseConnected = false;
		}
	}

	// ── Polling (fallback) ──────────────────────────────────────────────

	/** Start fallback polling for live updates (only used when SSE is unavailable). */
	function startPolling() {
		if (pollingIntervalId) return;
		pollingIntervalId = setInterval(async () => {
			await fetchNewEvents();
		}, POLLING_INTERVAL_MS);
		console.log(`[Lab:${netId}] Started fallback polling`);
	}

	/** Stop fallback polling. */
	function stopPolling() {
		if (pollingIntervalId) {
			clearInterval(pollingIntervalId);
			pollingIntervalId = null;
		}
	}

	// ── Live update orchestration ───────────────────────────────────────

	/** Start live updates: try SSE first, fall back to polling. */
	function startLiveUpdates() {
		connectSSE();
	}

	/** Stop all live updates (SSE + polling). */
	function stopLiveUpdates() {
		disconnectSSE();
		stopPolling();
		sseRetryCount = 0;
		console.log(`[Lab:${netId}] Stopped live updates`);
	}

	// ── Run mode ────────────────────────────────────────────────────────

	/** Fetch current run mode from backend and ensure SSE is connected. */
	async function fetchRunMode(): Promise<RunMode> {
		try {
			const response = await fetch(`${apiBase}/run-mode`);
			const data = await response.json();
			runMode = data.current_mode ?? 'stopped';
			// Always keep SSE connected regardless of run mode
			startLiveUpdates();
			return runMode;
		} catch (e) {
			console.error('Failed to fetch run mode:', e);
			return 'stopped';
		}
	}

	/** Set run mode on backend (running/stopped) */
	async function setRunMode(mode: RunMode): Promise<{ success: boolean; error?: string }> {
		try {
			const response = await fetch(`${apiBase}/run-mode`, {
				method: 'PUT',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ mode })
			});
			const data = await response.json();
			if (data.success) {
				runMode = data.current_mode;
				await fetchEvents();
				return { success: true };
			}
			return { success: false, error: data.error ?? 'Failed to set run mode' };
		} catch (e) {
			const msg = e instanceof Error ? e.message : 'Unknown error';
			return { success: false, error: msg };
		}
	}

	/** Force-hibernate this net: cancel eval loop and free memory. */
	async function hibernate(): Promise<{ success: boolean; error?: string }> {
		try {
			const response = await fetch(`${apiBase}/command/hibernate`, {
				method: 'POST'
			});
			const data = await response.json();
			if (data.success) {
				stopLiveUpdates();
				return { success: true };
			}
			return { success: false, error: data.error ?? 'Failed to hibernate net' };
		} catch (e) {
			const msg = e instanceof Error ? e.message : 'Unknown error';
			return { success: false, error: msg };
		}
	}

	/** One-shot evaluation: fire all enabled transitions until quiescent */
	async function evaluate(maxSteps: number = 1000): Promise<{
		success: boolean;
		stepsExecuted?: number;
		finalState?: 'quiescent' | 'limit_reached';
		error?: string;
	}> {
		if (evaluating) return { success: false, error: 'Evaluation already in progress' };
		evaluating = true;
		error = null;

		try {
			const response = await fetch(`${apiBase}/command/evaluate`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ max_steps: maxSteps })
			});
			const data = await response.json();

			if (data.success) {
				// Refresh events after evaluation
				await fetchEvents();
				replayIndex = events.length - 1;
				return {
					success: true,
					stepsExecuted: data.steps_executed,
					finalState: data.final_state
				};
			}
			return { success: false, error: data.error ?? 'Evaluation failed' };
		} catch (e) {
			const msg = e instanceof Error ? e.message : 'Unknown error';
			error = resolveErrorMessage(msg);
			return { success: false, error: msg };
		} finally {
			evaluating = false;
		}
	}

	// Get selected place details
	function getSelectedPlaceDetails() {
		const elem = selectedElement;
		if (!elem || elem.type !== 'place' || !topology) return null;
		const place = topology.places.find((p) => p.id === elem.id);
		if (!place) return null;
		const tokens = projectedMarking.get(place.id) ?? [];
		return { place, tokens };
	}

	// Get selected transition details
	function getSelectedTransitionDetails() {
		const elem = selectedElement;
		if (!elem || elem.type !== 'transition' || !topology) return null;
		const transition = topology.transitions.find((t) => t.id === elem.id);
		if (!transition) return null;
		const inputArcs = topology.arcs.filter(
			(a) => a.transition_id === transition.id && a.direction === 'place_to_transition'
		);
		const outputArcs = topology.arcs.filter(
			(a) => a.transition_id === transition.id && a.direction === 'transition_to_place'
		);
		return { transition, inputArcs, outputArcs };
	}

	// Get selected token details
	function getSelectedTokenDetails() {
		const elem = selectedElement;
		if (!elem || elem.type !== 'token') return null;
		const tokens = projectedMarking.get(elem.placeId) ?? [];
		let token = tokens.find((t) => t.id === elem.tokenId);
		// Also search bridged-out tokens (ghost tokens in outboxes)
		if (!token) {
			const bridged = bridgedOutTokens.get(elem.placeId) ?? [];
			token = bridged.find((t) => t.id === elem.tokenId);
		}
		if (!token) return null;
		const placeName = getPlaceName(elem.placeId);
		// Find the event that created this token
		const creationEvent = events.find((e) => {
			const eType: string = e.event.type;
			if (eType === 'TokenCreated') {
				return (e.event as any).token?.id === token.id;
			}
			if (eType === 'TransitionFired' || eType === 'EffectCompleted' || eType === 'EffectFailed') {
				const produced = (e.event as any).produced_tokens as [string, Token][];
				return produced?.some(([, t]) => t.id === token.id);
			}
			return false;
		});
		return { token, placeName, creationEvent };
	}

	// Get selected event details with resolved names
	function getSelectedEventDetails() {
		const elem = selectedElement;
		if (!elem || elem.type !== 'event') return null;
		const event = events.find((e) => e.sequence === elem.sequence);
		if (!event) return null;

		const details: {
			event: PersistedEvent;
			eventTypeName: string;
			transitionName?: string;
			placeName?: string;
			consumedTokens?: { placeId: string; placeName: string; tokenId: string }[];
			producedTokens?: { placeId: string; placeName: string; token: Token }[];
			token?: Token;
			errorMessage?: string;
			targetNetId?: string;
			targetPlaceName?: string;
			correlationId?: string;
			replyToPlaceName?: string;
			effectHandlerId?: string;
			effectResult?: unknown;
		} = {
			event,
			eventTypeName: event.event.type
		};

		switch (event.event.type as string) {
			case 'TransitionFired': {
				const e = event.event as any;
				details.transitionName = getTransitionName(e.transition_id);
				details.consumedTokens = (e.consumed_tokens as [string, string][])?.map(
					([placeId, tokenId]) => ({
						placeId,
						placeName: getPlaceName(placeId),
						tokenId
					})
				);
				details.producedTokens = (e.produced_tokens as [string, Token][])?.map(
					([placeId, token]) => ({
						placeId,
						placeName: getPlaceName(placeId),
						token
					})
				);
				break;
			}
			case 'TokenCreated': {
				const e = event.event as any;
				details.placeName = getPlaceName(e.place_id);
				details.token = e.token;
				break;
			}
			case 'TokenConsumed': {
				const e = event.event as any;
				details.placeName = getPlaceName(e.place_id);
				break;
			}
			case 'NetInitialized': {
				// No additional details needed
				break;
			}
			case 'TokenBridgedOut': {
				const e = event.event as any;
				details.transitionName = getTransitionName(e.transition_id);
				details.placeName = getPlaceName(e.source_place_id);
				details.token = e.token;
				details.targetNetId = e.target_net_id;
				details.targetPlaceName = e.target_place_name;
				details.correlationId = e.correlation_id;
				details.replyToPlaceName = e.reply_to_place_name ?? undefined;
				break;
			}
			case 'EffectCompleted': {
				const e = event.event as any;
				details.transitionName = getTransitionName(e.transition_id);
				details.effectHandlerId = e.effect_handler_id;
				details.effectResult = e.effect_result;
				details.consumedTokens = (e.consumed_tokens as [string, string][])?.map(
					([placeId, tokenId]) => ({
						placeId,
						placeName: getPlaceName(placeId),
						tokenId
					})
				);
				details.producedTokens = (e.produced_tokens as [string, Token][])?.map(
					([placeId, token]) => ({
						placeId,
						placeName: getPlaceName(placeId),
						token
					})
				);
				break;
			}
			case 'EffectFailed': {
				const e = event.event as any;
				details.transitionName = getTransitionName(e.transition_id);
				details.effectHandlerId = e.effect_handler_id;
				details.errorMessage = e.error_message ?? 'Effect failed';
				details.consumedTokens = (e.consumed_tokens as [string, string][])?.map(
					([placeId, tokenId]) => ({
						placeId,
						placeName: getPlaceName(placeId),
						tokenId
					})
				);
				details.producedTokens = (e.produced_tokens as [string, Token][])?.map(
					([placeId, token]) => ({
						placeId,
						placeName: getPlaceName(placeId),
						token
					})
				);
				break;
			}
			case 'ErrorOccurred': {
				const e = event.event as any;
				details.errorMessage = resolveErrorMessage(e.message ?? '');
				break;
			}
		}

		return details;
	}

	// Get selected group details (for collapsed meta-node inspector)
	function getSelectedGroupDetails() {
		const elem = selectedElement;
		if (!elem || elem.type !== 'group' || !topology) return null;
		const group = currentGroups.find(g => g.id === elem.id);
		if (!group) return null;

		// Collect all descendant group IDs (including self)
		const descIds = new Set<string>([group.id]);
		const queue = [group.id];
		while (queue.length > 0) {
			const gid = queue.shift()!;
			for (const g of currentGroups) {
				if (g.parent_id === gid && !descIds.has(g.id)) {
					descIds.add(g.id);
					queue.push(g.id);
				}
			}
		}

		// Find all places and transitions in this group hierarchy
		const places = topology.places.filter(p => {
			const gid = (p as any).group_id as string | undefined;
			return gid && descIds.has(gid);
		});
		const transitions = topology.transitions.filter(t => {
			const gid = (t as any).group_id as string | undefined;
			return gid && descIds.has(gid);
		});

		// Collect all tokens across contained places
		const allTokens: Array<{ placeId: string; placeName: string; token: Token }> = [];
		for (const place of places) {
			const tokens = projectedMarking.get(place.id) ?? [];
			for (const token of tokens) {
				allTokens.push({ placeId: place.id, placeName: place.name, token });
			}
		}

		// Nested child groups (direct children only)
		const childGroups = currentGroups.filter(g => g.parent_id === group.id);

		return { group, places, transitions, allTokens, childGroups };
	}

	// Return reactive getters and actions
	return {
		// Net identity
		netId,
		apiBase,
		get topology() {
			return topology;
		},
		get events() {
			return events;
		},
		get replayIndex() {
			return replayIndex;
		},
		get projectedMarking() {
			return projectedMarking;
		},
		get bridgedOutTokens() {
			return bridgedOutTokens;
		},
		get loading() {
			return loading;
		},
		get error() {
			return error;
		},
		get selectedElement() {
			return selectedElement;
		},
		get transitionStatuses() {
			return transitionStatuses;
		},
		// Run mode state (backend-controlled)
		get runMode() {
			return runMode;
		},
		get evaluating() {
			return evaluating;
		},
		// Current scenario (for adapter manager reference)
		get currentScenario() {
			return currentScenario;
		},
		// Groups from current scenario (for visualization)
		get groups() {
			return currentGroups;
		},
		// Static analysis
		get analysisReport() {
			return analysisReport;
		},
		// Services
		get services() {
			return services;
		},
		// Event spotlight (canvas highlighting when event selected)
		get eventSpotlight() {
			return eventSpotlight;
		},
		// Marking diff (pulse animation on single timeline step)
		get markingDiff() {
			return markingDiff;
		},
		// Actions
		fetchTopology,
		fetchEvents,
		fireTransition,
		reset,
		setReplayIndex,
		loadScenario,
		// Selection
		selectPlace,
		selectTransition,
		selectToken,
		selectEvent,
		selectGroup,
		selectRemoteNet,
		clearSelection,
		// Inspector helpers
		getSelectedPlaceDetails,
		getSelectedTransitionDetails,
		getSelectedTokenDetails,
		getSelectedEventDetails,
		getSelectedGroupDetails,
		// Token injection
		injectToken,
		// Name resolution
		getTransitionName,
		getPlaceName,
		// Run mode (backend-controlled execution)
		fetchRunMode,
		setRunMode,
		evaluate,
		// Hibernation
		hibernate,
		// Static analysis
		fetchAnalysis,
		// Services
		fetchServices,
		// Live updates (SSE + polling fallback)
		get sseConnected() {
			return sseConnected;
		},
		stopLiveUpdates,
	};
}
