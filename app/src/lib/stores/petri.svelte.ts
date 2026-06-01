/**
 * Petri net store for Mekhan.
 *
 * Connects to petri-lab's API for a specific net instance, provides reactive
 * state for the visualizer components (LabCanvas, Timeline, EventLog, Inspector).
 */

import { connectSse, type SseConnection } from '$lib/net/sse';
import { createPetriApi, PetriApiError } from '$lib/stores/petri-api';
import {
	computeEventSpotlight,
	computeMarkingDiff,
	projectBridgedOut,
	projectMarking
} from '$lib/stores/petri-projection';
import {
	getSelectedEventDetails as getSelectedEventDetails_,
	getSelectedGroupDetails as getSelectedGroupDetails_,
	getSelectedPlaceDetails as getSelectedPlaceDetails_,
	getSelectedTokenDetails as getSelectedTokenDetails_,
	getSelectedTransitionDetails as getSelectedTransitionDetails_
} from '$lib/stores/inspector-selectors';
import type {
	PetriNet,
	PersistedEvent,
	TransitionStatus,
	ScenarioGroup,
	SelectedElement,
	MarkingDiff,
	TokenColor
} from '$lib/types/petri';

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/** Base URL for petri-lab engine API. Override via PETRI_LAB_URL env or prop. */
const PETRI_BASE = '/petri';

// ---------------------------------------------------------------------------
// Store factory
// ---------------------------------------------------------------------------

export function createPetriStore(netId: string, baseUrl: string = PETRI_BASE) {
	const apiBase = `${baseUrl}/api/nets/${netId}`;
	const api = createPetriApi(apiBase);

	// ── Core state ──────────────────────────────────────────────────────
	let topology: PetriNet | null = $state(null);
	let events: PersistedEvent[] = $state([]);
	let replayIndex: number = $state(-1);
	let loading: boolean = $state(false);
	let error: string | null = $state(null);
	// Neutral terminal signal: the engine has tombstoned this net (completed
	// or cancelled — HTTP 409 on the event stream). NOT an error; the UI uses
	// it to stop hammering and show a calm "closed" state rather than a scary
	// failure. The completed-net replay/data view is a separate pending design.
	let netClosed: boolean = $state(false);
	let selectedElement: SelectedElement = $state(null);
	let transitionStatuses: Record<string, TransitionStatus> = $state({});
	let currentGroups: ScenarioGroup[] = $state([]);
	let lastFetchedSequence = 0;

	// ── Run mode ────────────────────────────────────────────────────────
	let runMode: string = $state('stopped');
	let evaluating: boolean = $state(false);

	// ── Analysis & services ─────────────────────────────────────────────
	type AnalysisReport = {
		is_valid: boolean;
		summary: { error_count: number; warning_count: number; info_count: number };
		issues: Array<{
			level: string;
			code: string;
			message: string;
			node_id: string;
			node_type: string;
		}>;
	};
	type Services = { handlers: string[]; categories: Record<string, string[]> };
	let analysisReport: AnalysisReport | null = $state(null);
	let services: Services | null = $state(null);

	// ── SSE ─────────────────────────────────────────────────────────────
	let sseConnection: SseConnection | null = null;
	const SSE_MAX_RETRIES = 5;
	const SSE_INITIAL_RETRY_MS = 1000;
	let pollInterval: ReturnType<typeof setInterval> | null = null;

	// ── Name maps ───────────────────────────────────────────────────────
	const transitionNameMap = $derived.by(() => {
		const map = new Map<string, string>();
		if (topology) {
			for (const t of topology.transitions) map.set(t.id, t.name);
		}
		return map;
	});

	const placeNameMap = $derived.by(() => {
		const map = new Map<string, string>();
		if (topology) {
			for (const p of topology.places) map.set(p.id, p.name);
		}
		return map;
	});

	function getTransitionName(id: string): string {
		return transitionNameMap.get(id) ?? id;
	}

	function getPlaceName(id: string): string {
		return placeNameMap.get(id) ?? id;
	}

	// Resolve UUIDs in error messages to human-readable names
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

	// ── Projected marking ───────────────────────────────────────────────
	const projectedMarking = $derived.by(() => projectMarking(events, replayIndex));

	const bridgedOutTokens = $derived.by(() => projectBridgedOut(events, replayIndex));

	// ── Event spotlight ─────────────────────────────────────────────────
	const eventSpotlight = $derived.by(() => {
		const sel = selectedElement;
		if (!sel || sel.type !== 'event') return null;
		return computeEventSpotlight(events, sel.sequence);
	});

	// ── Marking diff (for pulse animations) ─────────────────────────────
	let markingDiff: MarkingDiff | null = $state(null);

	// ── API functions ───────────────────────────────────────────────────
	//
	// Transport + typing lives in `petri-api.ts`, which throws a single
	// `PetriApiError` on any non-2xx. These wrappers keep each call site's
	// original error *policy*: fatal reads set `error`, non-critical reads
	// swallow, command helpers return `{ success }` with the response body.

	/** Extract the response body for `{ success }`-style helpers. */
	function failureText(e: unknown): string {
		if (e instanceof PetriApiError) return e.body;
		return e instanceof Error ? e.message : String(e);
	}

	async function fetchTopology() {
		try {
			const { topology: net, groups } = await api.fetchTopology();
			topology = net;
			currentGroups = groups;
		} catch (e: any) {
			error = `Failed to fetch topology: ${e.message}`;
		}
	}

	async function fetchEvents() {
		try {
			const raw = await api.fetchEvents();
			// Deduplicate by sequence (backend may emit duplicates)
			const seen = new Set<number>();
			events = raw.filter((e) => {
				if (seen.has(e.sequence)) return false;
				seen.add(e.sequence);
				return true;
			});
			if (events.length > 0) {
				// Only jump to end on initial load (replayIndex not yet set)
				if (replayIndex < 0) {
					replayIndex = events.length - 1;
				}
				lastFetchedSequence = events[events.length - 1].sequence;
			}
			await fetchState();
		} catch (e: any) {
			error = `Failed to fetch events: ${e.message}`;
		}
	}

	async function fetchNewEvents() {
		try {
			const newEvents = await api.fetchEvents(lastFetchedSequence + 1);
			if (newEvents.length > 0) {
				// Deduplicate by sequence
				const existingSeqs = new Set(events.map((e) => e.sequence));
				const unique = newEvents.filter((e) => !existingSeqs.has(e.sequence));
				if (unique.length > 0) {
					// Only auto-advance if the user is following the live tail
					const wasAtEnd = replayIndex >= events.length - 1;
					events = [...events, ...unique];
					if (wasAtEnd) {
						replayIndex = events.length - 1;
					}
					lastFetchedSequence = events[events.length - 1].sequence;
					await fetchState();
				}
			}
		} catch {
			// Silently retry on next poll
		}
	}

	async function fetchState() {
		try {
			const statuses = await api.fetchState();
			if (statuses) {
				transitionStatuses = statuses as Record<string, TransitionStatus>;
			}
		} catch {
			// Non-critical
		}
	}

	async function fetchRunMode() {
		try {
			runMode = await api.fetchRunMode();
		} catch {
			// Non-critical
		}
	}

	// ── Commands ────────────────────────────────────────────────────────

	async function fireTransition(transitionId: string) {
		try {
			await api.fireTransition(transitionId);
			await fetchEvents();
		} catch (e: any) {
			error = `Failed to fire transition: ${e.message}`;
		}
	}

	async function injectToken(placeId: string, data: unknown): Promise<{ success: boolean; error?: string }> {
		try {
			const color: TokenColor = data == null ? { type: 'Unit' } : { type: 'Data', value: data };
			await api.createToken(placeId, color);
			await fetchEvents();
			return { success: true };
		} catch (e: any) {
			return { success: false, error: failureText(e) };
		}
	}

	async function evaluate(maxSteps?: number) {
		evaluating = true;
		try {
			await api.evaluate(maxSteps ?? 100);
			await fetchEvents();
		} catch (e: any) {
			error = `Evaluate failed: ${e.message}`;
		} finally {
			evaluating = false;
		}
	}

	async function reset() {
		try {
			await api.reset();
			events = [];
			replayIndex = -1;
			lastFetchedSequence = 0;
			transitionStatuses = {};
			selectedElement = null;
			await fetchTopology();
			await fetchEvents();
		} catch (e: any) {
			error = `Reset failed: ${e.message}`;
		}
	}

	async function setRunMode(mode: string) {
		try {
			await api.setRunMode(mode);
			runMode = mode;
			await fetchEvents();
		} catch (e: any) {
			error = `Failed to set run mode: ${e.message}`;
		}
	}

	async function hibernate() {
		try {
			await api.hibernate();
			stopLiveUpdates();
		} catch (e: any) {
			error = `Hibernate failed: ${e.message}`;
		}
	}

	async function fetchAnalysis() {
		try {
			analysisReport = await api.fetchAnalysis<AnalysisReport>();
		} catch {
			// Non-critical
		}
	}

	async function fetchServices() {
		try {
			services = await api.fetchServices<Services>();
		} catch {
			// Non-critical
		}
	}

	async function loadScenario(scenario: unknown): Promise<{ success: boolean; error?: string; places_count?: number; transitions_count?: number; tokens_count?: number }> {
		try {
			// Envelope-aware: the transport in `petri-api.ts::loadScenario` wraps the
			// scenario in `LoadScenarioRequest { scenario, skip_mask?, stage_overrides? }`
			// per the sub-phase 2.5e-γ.mekhan-S3 cutover. The frontend editor does not
			// drive ablation; the wrap omits skip_mask/stage_overrides and engine serde
			// deserialises them as empty.
			const data = await api.loadScenario(scenario);
			return {
				success: true,
				places_count: data.places_count,
				transitions_count: data.transitions_count,
				tokens_count: data.tokens_count
			};
		} catch (e: any) {
			return { success: false, error: failureText(e) };
		}
	}

	async function saveTransitionScript(transitionId: string, script: string, guard: string | null) {
		try {
			await api.saveTransitionScript(transitionId, script, guard);
		} catch (e: any) {
			// Preserve the original convention: rethrow the response body text.
			throw new Error(failureText(e));
		}
		await fetchTopology();
		await fetchEvents();
	}

	// ── Timeline & selection ────────────────────────────────────────────

	function setReplayIndex(index: number) {
		// Compute marking diff for pulse animation (adjacent single-step moves).
		const diff = computeMarkingDiff(events, replayIndex, index);
		if (diff) {
			markingDiff = diff;
			setTimeout(() => {
				markingDiff = null;
			}, 700);
		} else {
			markingDiff = null;
		}

		replayIndex = index;
	}

	function selectPlace(id: string) { selectedElement = { type: 'place', id }; }
	function selectTransition(id: string) { selectedElement = { type: 'transition', id }; }
	function selectToken(placeId: string, tokenId: string) { selectedElement = { type: 'token', placeId, tokenId }; }
	function selectEvent(sequence: number) { selectedElement = { type: 'event', sequence }; }
	function selectGroup(id: string) { selectedElement = { type: 'group', id }; }
	function selectRemoteNet(id: string, label: string, targets: string[], sources: string[], childNetIds: string[]) {
		selectedElement = { type: 'remotenet', id, label, targets, sources, childNetIds };
	}
	function clearSelection() { selectedElement = null; }

	// ── Inspector helpers ───────────────────────────────────────────────

	// These delegate to the pure selectors in `inspector-selectors.ts`; the
	// store only supplies the current reactive inputs.
	function getSelectedPlaceDetails() {
		return getSelectedPlaceDetails_(selectedElement, topology, projectedMarking);
	}

	function getSelectedTransitionDetails() {
		return getSelectedTransitionDetails_(selectedElement, topology, getPlaceName);
	}

	function getSelectedTokenDetails() {
		return getSelectedTokenDetails_(
			selectedElement,
			projectedMarking,
			bridgedOutTokens,
			events,
			getPlaceName
		);
	}

	function getSelectedEventDetails() {
		return getSelectedEventDetails_(
			selectedElement,
			events,
			getTransitionName,
			getPlaceName,
			resolveErrorMessage
		);
	}

	function getSelectedGroupDetails() {
		return getSelectedGroupDetails_(selectedElement, currentGroups);
	}

	// ── SSE live updates ────────────────────────────────────────────────

	function connectSSE() {
		sseConnection?.close();
		sseConnection = connectSse(
			() => `${apiBase}/events/stream?from_sequence=${lastFetchedSequence + 1}`,
			{
				maxRetries: SSE_MAX_RETRIES,
				initialRetryMs: SSE_INITIAL_RETRY_MS,
				// After the retry budget is spent, fall back to polling.
				onRetriesExhausted: () => startPolling(),
				// Terminal client error (esp. 409 "Net is completed or
				// cancelled"): retrying/polling can never succeed and would
				// 409 forever. Stop cleanly with a neutral closed signal —
				// no polling, no fatal error.
				onTerminal: () => {
					disconnectSSE();
					stopPolling();
					netClosed = true;
				},
				onEvent: ({ event, data }) => handleSSEMessage(event, data)
			}
		);
	}

	function handleSSEMessage(type: string, data: string) {
		try {
			if (type === 'update') {
				const parsed = JSON.parse(data);
				const newEvents: PersistedEvent[] = Array.isArray(parsed) ? parsed : [parsed];
				const existingSeqs = new Set(events.map((e) => e.sequence));
				const unique = newEvents.filter((e) => !existingSeqs.has(e.sequence));
				if (unique.length > 0) {
					// Only auto-advance if the user is following the live tail
					const wasAtEnd = replayIndex >= events.length - 1;
					events = [...events, ...unique];
					if (wasAtEnd) {
						replayIndex = events.length - 1;
					}
					lastFetchedSequence = events[events.length - 1].sequence;
					fetchState();
				}
			} else if (type === 'reset') {
				events = [];
				replayIndex = -1;
				lastFetchedSequence = 0;
				fetchTopology();
				fetchEvents();
			}
		} catch {
			// Malformed SSE data, skip
		}
	}

	function disconnectSSE() {
		sseConnection?.close();
		sseConnection = null;
	}

	function startPolling() {
		stopPolling();
		pollInterval = setInterval(() => fetchNewEvents(), 500);
	}

	function stopPolling() {
		if (pollInterval) {
			clearInterval(pollInterval);
			pollInterval = null;
		}
	}

	function startLiveUpdates() {
		connectSSE();
	}

	function stopLiveUpdates() {
		disconnectSSE();
		stopPolling();
	}

	// ── Initialization ──────────────────────────────────────────────────

	async function init() {
		loading = true;
		error = null;
		try {
			await fetchTopology();
			await fetchEvents();
			await fetchRunMode();
			await Promise.all([fetchAnalysis(), fetchServices()]);
			startLiveUpdates();
		} catch (e: any) {
			error = `Initialization failed: ${e.message}`;
		} finally {
			loading = false;
		}
	}

	function destroy() {
		stopLiveUpdates();
	}

	// ── Public interface ────────────────────────────────────────────────

	return {
		// Reactive state (read via getters)
		get topology() { return topology; },
		get events() { return events; },
		get replayIndex() { return replayIndex; },
		get projectedMarking() { return projectedMarking; },
		get bridgedOutTokens() { return bridgedOutTokens; },
		get eventSpotlight() { return eventSpotlight; },
		get markingDiff() { return markingDiff; },
		get loading() { return loading; },
		get error() { return error; },
		get netClosed() { return netClosed; },
		get selectedElement() { return selectedElement; },
		get transitionStatuses() { return transitionStatuses; },
		get currentGroups() { return currentGroups; },
		get runMode() { return runMode; },
		get evaluating() { return evaluating; },
		get analysisReport() { return analysisReport; },
		get services() { return services; },
		get apiBase() { return apiBase; },

		// Name resolution
		getTransitionName,
		getPlaceName,

		// Inspector helpers
		getSelectedPlaceDetails,
		getSelectedTransitionDetails,
		getSelectedTokenDetails,
		getSelectedEventDetails,
		getSelectedGroupDetails,

		// Actions
		fireTransition,
		injectToken,
		evaluate,
		reset,
		setRunMode,
		hibernate,
		saveTransitionScript,
		fetchAnalysis,
		fetchServices,
		loadScenario,

		// Timeline & selection
		setReplayIndex,
		selectPlace,
		selectTransition,
		selectToken,
		selectEvent,
		selectGroup,
		selectRemoteNet,
		clearSelection,

		// Lifecycle
		init,
		destroy,
		startLiveUpdates,
		stopLiveUpdates
	};
}

export type PetriStore = ReturnType<typeof createPetriStore>;
