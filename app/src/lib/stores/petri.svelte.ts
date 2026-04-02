/**
 * Petri net store for Mekhan.
 *
 * Connects to petri-lab's API for a specific net instance, provides reactive
 * state for the visualizer components (LabCanvas, Timeline, EventLog, Inspector).
 *
 * Based on petri-lab/lab-ui's createLabStore but adapted for Mekhan's context.
 */

import type {
	PetriNet,
	Token,
	PersistedEvent,
	DomainEvent,
	TransitionStatus,
	ScenarioGroup,
	SelectedElement,
	EventSpotlight,
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

	// ── Core state ──────────────────────────────────────────────────────
	let topology: PetriNet | null = $state(null);
	let events: PersistedEvent[] = $state([]);
	let replayIndex: number = $state(-1);
	let loading: boolean = $state(false);
	let error: string | null = $state(null);
	let selectedElement: SelectedElement = $state(null);
	let transitionStatuses: Record<string, TransitionStatus> = $state({});
	let currentGroups: ScenarioGroup[] = $state([]);
	let lastFetchedSequence = 0;

	// ── Run mode ────────────────────────────────────────────────────────
	let runMode: string = $state('stopped');
	let evaluating: boolean = $state(false);

	// ── Analysis & services ─────────────────────────────────────────────
	let analysisReport: { is_valid: boolean; summary: { error_count: number; warning_count: number; info_count: number }; issues: Array<{ level: string; code: string; message: string; node_id: string; node_type: string }> } | null = $state(null);
	let services: { handlers: string[]; categories: Record<string, string[]> } | null = $state(null);

	// ── SSE ─────────────────────────────────────────────────────────────
	let sseAbortController: AbortController | null = null;
	let sseRetryCount = 0;
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

	// ── Projected marking ───────────────────────────────────────────────
	const projectedMarking = $derived.by(() => {
		const marking = new Map<string, Token[]>();
		if (!events.length) return marking;

		const end = Math.min(replayIndex + 1, events.length);
		for (let i = 0; i < end; i++) {
			const ev = events[i].event;
			applyEventToMarking(marking, ev);
		}
		return marking;
	});

	const bridgedOutTokens = $derived.by(() => {
		const bridged = new Map<string, Token[]>();
		if (!events.length) return bridged;

		const end = Math.min(replayIndex + 1, events.length);
		for (let i = 0; i < end; i++) {
			const ev = events[i].event;
			if (ev.type === 'TokenBridgedOut') {
				const tokens = bridged.get(ev.source_place_id) ?? [];
				tokens.push(ev.token);
				bridged.set(ev.source_place_id, tokens);
			}
		}
		return bridged;
	});

	// ── Event spotlight ─────────────────────────────────────────────────
	const eventSpotlight = $derived.by((): EventSpotlight | null => {
		const sel = selectedElement;
		if (!sel || sel.type !== 'event') return null;
		const ev = events.find((e) => e.sequence === sel.sequence);
		if (!ev) return null;

		const consumedPlaceIds: string[] = [];
		const producedPlaceIds: string[] = [];
		let transitionId: string | null = null;
		let targetPlaceId: string | null = null;

		const domainEvent = ev.event;
		if (
			domainEvent.type === 'TransitionFired' ||
			domainEvent.type === 'EffectCompleted' ||
			domainEvent.type === 'EffectFailed'
		) {
			transitionId = domainEvent.transition_id;
			if ('consumed_tokens' in domainEvent) {
				for (const [placeId] of domainEvent.consumed_tokens) {
					consumedPlaceIds.push(placeId);
				}
			}
			if ('produced_tokens' in domainEvent) {
				for (const [placeId] of domainEvent.produced_tokens) {
					producedPlaceIds.push(placeId);
				}
			}
		} else if (domainEvent.type === 'TokenCreated') {
			targetPlaceId = domainEvent.place_id;
		} else if (domainEvent.type === 'TokenBridgedOut') {
			targetPlaceId = domainEvent.source_place_id;
		}

		const allNodeIds = [
			...consumedPlaceIds,
			...producedPlaceIds,
			...(transitionId ? [transitionId] : []),
			...(targetPlaceId ? [targetPlaceId] : [])
		];

		return { transitionId, consumedPlaceIds, producedPlaceIds, targetPlaceId, allNodeIds };
	});

	// ── Marking diff (for pulse animations) ─────────────────────────────
	let markingDiff: MarkingDiff | null = $state(null);

	// ── API functions ───────────────────────────────────────────────────

	async function fetchTopology() {
		try {
			const res = await fetch(`${apiBase}/topology`);
			if (!res.ok) throw new Error(`${res.status}`);
			const data = await res.json();
			// Engine returns TopologyResponse: { topology: { places, transitions, arcs, groups } }
			const net = data.topology ?? data.net ?? data;
			topology = net;
			currentGroups = net?.groups ?? data.groups ?? [];
		} catch (e: any) {
			error = `Failed to fetch topology: ${e.message}`;
		}
	}

	async function fetchEvents() {
		try {
			const res = await fetch(`${apiBase}/events`);
			if (!res.ok) throw new Error(`${res.status}`);
			const data = await res.json();
			events = data.events ?? [];
			if (events.length > 0) {
				replayIndex = events.length - 1;
				lastFetchedSequence = events[events.length - 1].sequence;
			}
			await fetchState();
		} catch (e: any) {
			error = `Failed to fetch events: ${e.message}`;
		}
	}

	async function fetchNewEvents() {
		try {
			const res = await fetch(`${apiBase}/events?from_sequence=${lastFetchedSequence + 1}`);
			if (!res.ok) return;
			const data = await res.json();
			const newEvents: PersistedEvent[] = data.events ?? [];
			if (newEvents.length > 0) {
				// Deduplicate by sequence
				const existingSeqs = new Set(events.map((e) => e.sequence));
				const unique = newEvents.filter((e) => !existingSeqs.has(e.sequence));
				if (unique.length > 0) {
					events = [...events, ...unique];
					replayIndex = events.length - 1;
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
			const res = await fetch(`${apiBase}/state`);
			if (!res.ok) return;
			const data = await res.json();
			if (data.transition_statuses) {
				transitionStatuses = data.transition_statuses;
			}
		} catch {
			// Non-critical
		}
	}

	async function fetchRunMode() {
		try {
			const res = await fetch(`${apiBase}/run-mode`);
			if (!res.ok) return;
			const data = await res.json();
			runMode = data.mode ?? 'stopped';
		} catch {
			// Non-critical
		}
	}

	// ── Commands ────────────────────────────────────────────────────────

	async function fireTransition(transitionId: string) {
		try {
			await fetch(`${apiBase}/command/fire/${transitionId}`, { method: 'POST' });
			await fetchEvents();
		} catch (e: any) {
			error = `Failed to fire transition: ${e.message}`;
		}
	}

	async function injectToken(placeId: string, data: unknown): Promise<{ success: boolean; error?: string }> {
		try {
			const color: TokenColor = data == null ? { type: 'Unit' } : { type: 'Data', value: data };
			const res = await fetch(`${apiBase}/command/create-token`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ place_id: placeId, color })
			});
			if (!res.ok) {
				const body = await res.text();
				return { success: false, error: body };
			}
			await fetchEvents();
			return { success: true };
		} catch (e: any) {
			return { success: false, error: e.message };
		}
	}

	async function evaluate(maxSteps?: number) {
		evaluating = true;
		try {
			const res = await fetch(`${apiBase}/command/evaluate`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ max_steps: maxSteps ?? 100 })
			});
			if (!res.ok) throw new Error(`${res.status}`);
			await fetchEvents();
		} catch (e: any) {
			error = `Evaluate failed: ${e.message}`;
		} finally {
			evaluating = false;
		}
	}

	async function reset() {
		try {
			await fetch(`${apiBase}/command/reset`, { method: 'POST' });
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
			await fetch(`${apiBase}/run-mode`, {
				method: 'PUT',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ mode })
			});
			runMode = mode;
			await fetchEvents();
		} catch (e: any) {
			error = `Failed to set run mode: ${e.message}`;
		}
	}

	async function hibernate() {
		try {
			await fetch(`${apiBase}/command/hibernate`, { method: 'POST' });
			stopLiveUpdates();
		} catch (e: any) {
			error = `Hibernate failed: ${e.message}`;
		}
	}

	async function fetchAnalysis() {
		try {
			const res = await fetch(`${apiBase}/analysis`);
			if (!res.ok) return;
			analysisReport = await res.json();
		} catch {
			// Non-critical
		}
	}

	async function fetchServices() {
		try {
			const res = await fetch(`${apiBase}/services`);
			if (!res.ok) return;
			services = await res.json();
		} catch {
			// Non-critical
		}
	}

	async function loadScenario(scenario: unknown): Promise<{ success: boolean; error?: string; places_count?: number; transitions_count?: number; tokens_count?: number }> {
		try {
			const res = await fetch(`${apiBase}/scenario`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify(scenario)
			});
			if (!res.ok) {
				const body = await res.text();
				return { success: false, error: body };
			}
			const data = await res.json();
			return { success: true, places_count: data.places_count, transitions_count: data.transitions_count, tokens_count: data.tokens_count };
		} catch (e: any) {
			return { success: false, error: e.message };
		}
	}

	async function saveTransitionScript(transitionId: string, script: string, guard: string | null) {
		const res = await fetch(`${apiBase}/topology/transition/${transitionId}`, {
			method: 'PATCH',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ script, guard })
		});
		if (!res.ok) {
			const body = await res.text();
			throw new Error(body);
		}
		await fetchTopology();
		await fetchEvents();
	}

	// ── Timeline & selection ────────────────────────────────────────────

	function setReplayIndex(index: number) {
		// Compute marking diff for pulse animation
		if (Math.abs(index - replayIndex) === 1 && index >= 0 && index < events.length) {
			const ev = events[index > replayIndex ? index : replayIndex].event;
			const appeared: string[] = [];
			const disappeared: string[] = [];
			let firedTransition: string | null = null;

			if (
				ev.type === 'TransitionFired' ||
				ev.type === 'EffectCompleted'
			) {
				firedTransition = ev.transition_id;
				if (index > replayIndex) {
					// Stepping forward
					for (const [placeId] of ev.consumed_tokens) disappeared.push(placeId);
					for (const [placeId] of ev.produced_tokens) appeared.push(placeId);
				} else {
					// Stepping backward
					for (const [placeId] of ev.consumed_tokens) appeared.push(placeId);
					for (const [placeId] of ev.produced_tokens) disappeared.push(placeId);
				}
			} else if (ev.type === 'TokenCreated') {
				if (index > replayIndex) appeared.push(ev.place_id);
				else disappeared.push(ev.place_id);
			}

			markingDiff = { appeared, disappeared, firedTransition };
			setTimeout(() => { markingDiff = null; }, 700);
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

	function getSelectedPlaceDetails() {
		const sel = selectedElement;
		if (!sel || sel.type !== 'place' || !topology) return null;
		const place = topology.places.find((p) => p.id === sel.id);
		if (!place) return null;
		const tokens = projectedMarking.get(place.id) ?? [];
		return { place, tokens };
	}

	function getSelectedTransitionDetails() {
		const sel = selectedElement;
		if (!sel || sel.type !== 'transition' || !topology) return null;
		const transition = topology.transitions.find((t) => t.id === sel.id);
		if (!transition) return null;
		const inputArcs = topology.arcs
			.filter((a) => a.transition_id === transition.id && a.direction === 'place_to_transition')
			.map((a) => ({ place_id: a.place_id, place_name: getPlaceName(a.place_id), weight: a.weight }));
		const outputArcs = topology.arcs
			.filter((a) => a.transition_id === transition.id && a.direction === 'transition_to_place')
			.map((a) => ({ place_id: a.place_id, place_name: getPlaceName(a.place_id), weight: a.weight }));
		return { transition, inputArcs, outputArcs };
	}

	function getSelectedTokenDetails() {
		const sel = selectedElement;
		if (!sel || sel.type !== 'token') return null;
		const tokens = projectedMarking.get(sel.placeId) ?? [];
		const token = tokens.find((t) => t.id === sel.tokenId);
		if (!token) return null;
		const placeName = getPlaceName(sel.placeId);
		const creationEvent = events.find(
			(e) => e.event.type === 'TokenCreated' && (e.event as any).token?.id === token.id
		);
		return { token, placeName, creationEvent };
	}

	function getSelectedEventDetails() {
		const sel = selectedElement;
		if (!sel || sel.type !== 'event') return null;
		return events.find((e) => e.sequence === sel.sequence) ?? null;
	}

	function getSelectedGroupDetails() {
		const sel = selectedElement;
		if (!sel || sel.type !== 'group') return null;
		const group = currentGroups.find((g) => g.id === sel.id);
		if (!group) return null;
		return { group };
	}

	// ── SSE live updates ────────────────────────────────────────────────

	function connectSSE() {
		if (sseAbortController) sseAbortController.abort();
		sseAbortController = new AbortController();

		const fromSeq = lastFetchedSequence + 1;
		const url = `${apiBase}/events/stream?from_sequence=${fromSeq}`;

		fetch(url, { signal: sseAbortController.signal })
			.then((response) => {
				if (!response.ok || !response.body) {
					throw new Error(`SSE failed: ${response.status}`);
				}
				sseRetryCount = 0;
				const reader = response.body.getReader();
				const decoder = new TextDecoder();
				let buffer = '';

				function processChunk(): Promise<void> {
					return reader.read().then(({ done, value }) => {
						if (done) {
							// Stream ended — reconnect
							scheduleSSERetry();
							return;
						}
						buffer += decoder.decode(value, { stream: true });
						const lines = buffer.split('\n');
						buffer = lines.pop() ?? '';

						let eventType = '';
						let eventData = '';

						for (const line of lines) {
							if (line.startsWith('event: ')) {
								eventType = line.slice(7).trim();
							} else if (line.startsWith('data: ')) {
								eventData = line.slice(6);
							} else if (line === '' && eventData) {
								handleSSEMessage(eventType, eventData);
								eventType = '';
								eventData = '';
							}
						}
						return processChunk();
					});
				}

				return processChunk();
			})
			.catch((err) => {
				if (err.name !== 'AbortError') {
					scheduleSSERetry();
				}
			});
	}

	function handleSSEMessage(type: string, data: string) {
		try {
			if (type === 'update') {
				const parsed = JSON.parse(data);
				const newEvents: PersistedEvent[] = Array.isArray(parsed) ? parsed : [parsed];
				const existingSeqs = new Set(events.map((e) => e.sequence));
				const unique = newEvents.filter((e) => !existingSeqs.has(e.sequence));
				if (unique.length > 0) {
					events = [...events, ...unique];
					replayIndex = events.length - 1;
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

	function scheduleSSERetry() {
		if (sseRetryCount >= SSE_MAX_RETRIES) {
			// Fall back to polling
			startPolling();
			return;
		}
		const delay = SSE_INITIAL_RETRY_MS * Math.pow(2, sseRetryCount);
		sseRetryCount++;
		setTimeout(() => connectSSE(), delay);
	}

	function disconnectSSE() {
		if (sseAbortController) {
			sseAbortController.abort();
			sseAbortController = null;
		}
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function applyEventToMarking(marking: Map<string, Token[]>, ev: DomainEvent) {
	switch (ev.type) {
		case 'TokenCreated': {
			const tokens = marking.get(ev.place_id) ?? [];
			tokens.push(ev.token);
			marking.set(ev.place_id, tokens);
			break;
		}
		case 'TransitionFired':
		case 'EffectCompleted': {
			// Remove consumed tokens
			for (const [placeId, tokenId] of ev.consumed_tokens) {
				const tokens = marking.get(placeId);
				if (tokens) {
					const idx = tokens.findIndex((t) => t.id === tokenId);
					if (idx >= 0) tokens.splice(idx, 1);
					if (tokens.length === 0) marking.delete(placeId);
				}
			}
			// Add produced tokens
			for (const [placeId, token] of ev.produced_tokens) {
				const tokens = marking.get(placeId) ?? [];
				tokens.push(token);
				marking.set(placeId, tokens);
			}
			break;
		}
		case 'TokenConsumed':
		case 'TokenRemoved': {
			const tokens = marking.get(ev.place_id);
			if (tokens) {
				const idx = tokens.findIndex((t) => t.id === ev.token_id);
				if (idx >= 0) tokens.splice(idx, 1);
				if (tokens.length === 0) marking.delete(ev.place_id);
			}
			break;
		}
		case 'TokenBridgedOut': {
			// Token leaves the local marking
			const tokens = marking.get(ev.source_place_id);
			if (tokens) {
				const idx = tokens.findIndex((t) => t.id === ev.token.id);
				if (idx >= 0) tokens.splice(idx, 1);
				if (tokens.length === 0) marking.delete(ev.source_place_id);
			}
			break;
		}
	}
}
