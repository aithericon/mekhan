/**
 * Pure projection helpers for the petri store.
 *
 * These contain no reactive state and no I/O — they fold the persisted event
 * log into derived views (marking, bridged-out tokens, event spotlight,
 * marking diff). Extracted verbatim from `petri.svelte.ts` so the store can
 * stay a thin orchestrator and so this logic is unit-testable in isolation.
 */

import type {
	DomainEvent,
	EventSpotlight,
	MarkingDiff,
	PersistedEvent,
	Token
} from '$lib/types/petri';

/**
 * Apply a single domain event to a mutable marking map (place id → tokens).
 * Mirrors petri-domain's reducer for the subset of events that move tokens.
 */
export function applyEventToMarking(marking: Map<string, Token[]>, ev: DomainEvent): void {
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

/** Fold events `[0, replayIndex]` into the place→tokens marking map. */
export function projectMarking(
	events: PersistedEvent[],
	replayIndex: number
): Map<string, Token[]> {
	const marking = new Map<string, Token[]>();
	if (!events.length) return marking;
	const end = Math.min(replayIndex + 1, events.length);
	for (let i = 0; i < end; i++) {
		applyEventToMarking(marking, events[i].event);
	}
	return marking;
}

/** Collect tokens that have been bridged out, keyed by source place id. */
export function projectBridgedOut(
	events: PersistedEvent[],
	replayIndex: number
): Map<string, Token[]> {
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
}

/**
 * Compute which nodes a selected event "spotlights" — the consumed/produced
 * places, the firing transition and any token target — for canvas highlight.
 */
export function computeEventSpotlight(
	events: PersistedEvent[],
	sequence: number
): EventSpotlight | null {
	const ev = events.find((e) => e.sequence === sequence);
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
		if ('consumed_tokens' in domainEvent && domainEvent.consumed_tokens) {
			for (const [placeId] of domainEvent.consumed_tokens) {
				consumedPlaceIds.push(placeId);
			}
		}
		if ('produced_tokens' in domainEvent && domainEvent.produced_tokens) {
			for (const [placeId] of domainEvent.produced_tokens) {
				producedPlaceIds.push(placeId);
			}
		}
	} else if (domainEvent.type === 'TokenCreated') {
		targetPlaceId = domainEvent.place_id;
	} else if (domainEvent.type === 'TokenBridgedOut') {
		if (domainEvent.transition_id) transitionId = domainEvent.transition_id;
		consumedPlaceIds.push(domainEvent.source_place_id);
	}

	const allNodeIds = [
		...consumedPlaceIds,
		...producedPlaceIds,
		...(transitionId ? [transitionId] : []),
		...(targetPlaceId ? [targetPlaceId] : [])
	];

	return { transitionId, consumedPlaceIds, producedPlaceIds, targetPlaceId, allNodeIds };
}

/**
 * Compute the appeared/disappeared/fired diff between two adjacent replay
 * indices (used to drive pulse animations). Returns `null` when the step is
 * not a single adjacent move or is out of range.
 */
export function computeMarkingDiff(
	events: PersistedEvent[],
	fromIndex: number,
	toIndex: number
): MarkingDiff | null {
	if (
		Math.abs(toIndex - fromIndex) !== 1 ||
		toIndex < 0 ||
		toIndex >= events.length
	) {
		return null;
	}

	const ev = events[toIndex > fromIndex ? toIndex : fromIndex].event;
	const appeared: string[] = [];
	const disappeared: string[] = [];
	let firedTransition: string | null = null;
	const forward = toIndex > fromIndex;

	if (
		ev.type === 'TransitionFired' ||
		ev.type === 'EffectCompleted' ||
		ev.type === 'EffectFailed'
	) {
		firedTransition = ev.transition_id;
		const consumed = 'consumed_tokens' in ev && ev.consumed_tokens ? ev.consumed_tokens : [];
		const produced = 'produced_tokens' in ev && ev.produced_tokens ? ev.produced_tokens : [];
		if (forward) {
			for (const [placeId] of consumed) disappeared.push(placeId);
			for (const [placeId] of produced) appeared.push(placeId);
		} else {
			for (const [placeId] of consumed) appeared.push(placeId);
			for (const [placeId] of produced) disappeared.push(placeId);
		}
	} else if (ev.type === 'TokenCreated') {
		if (forward) appeared.push(ev.place_id);
		else disappeared.push(ev.place_id);
	} else if (ev.type === 'TokenConsumed' || ev.type === 'TokenRemoved') {
		if (forward) disappeared.push(ev.place_id);
		else appeared.push(ev.place_id);
	} else if (ev.type === 'TokenBridgedOut') {
		if (ev.transition_id) firedTransition = ev.transition_id;
		if (forward) disappeared.push(ev.source_place_id);
		else appeared.push(ev.source_place_id);
	}

	return { appeared, disappeared, firedTransition };
}
