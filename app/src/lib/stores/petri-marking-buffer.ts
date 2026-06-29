/**
 * Bounded event buffer with incremental marking — the scalability fix for the
 * petri viewer.
 *
 * The engine streams an unbounded event log. Holding every event in memory AND
 * re-folding the whole marking on each append is O(n)-per-event with n growing
 * without bound, which froze the browser tab and grew its memory linearly on a
 * busy net. This buffer mirrors the engine's own base+tail design:
 *
 * - a bounded **tail** of recent events (`events`) for the log/timeline,
 * - a folded **base** marking for everything evicted past the cap,
 * - a **live** marking maintained incrementally as events arrive.
 *
 * Steady-state cost is O(#places)/event (the incremental apply), not O(events).
 * Following the live tail returns a shallow copy of the live marking; a manual
 * scrub folds the bounded retained window onto the evicted base.
 *
 * Pure (no Svelte runes, no I/O) so it is unit-testable in isolation; the
 * reactive `petri.svelte.ts` store delegates to it and mirrors its snapshot
 * into `$state`.
 */

import type { PersistedEvent, Token } from '$lib/types/petri';
import { applyEventToMarking } from './petri-projection';

/** Default retained-tail size. ~5000 events keeps the timeline/log useful while
 *  bounding both memory and per-update projection cost. */
export const DEFAULT_EVENT_BUFFER_CAP = 5000;

export interface MarkingView {
	/** place id → live tokens, for the active replay cursor. */
	marking: Map<string, Token[]>;
	/** source place id → tokens that have bridged out. */
	bridgedOut: Map<string, Token[]>;
}

export interface MarkingBuffer {
	/** Append streamed events (ascending sequence). Dedupes by sequence, applies
	 *  each to the live marking, trims to the cap, and (if following the live
	 *  tail) advances the cursor. Returns true iff any new event was applied. */
	append(incoming: PersistedEvent[]): boolean;
	/** Clear all state (net reset / full reload). */
	reset(): void;
	/** Move the replay cursor (an index into the retained `events` buffer). */
	setReplayIndex(index: number): void;
	/** The retained event tail (front-evicted past the cap). */
	readonly events: PersistedEvent[];
	/** Replay cursor: index into `events`; the last index means "following live". */
	readonly replayIndex: number;
	/** Count of events dropped from the front (history no longer scrubbable). */
	readonly evictedCount: number;
	/** Highest event sequence seen — the cursor for the next incremental fetch.
	 *  `-1` when empty, so `lastSequence + 1` is the right `from_sequence`. */
	readonly lastSequence: number;
	/** Marking view for the current cursor. Returns fresh top-level Map refs so
	 *  reactive consumers re-derive; token arrays may be shared with the live
	 *  state but consumers rebuild off the new Map ref. */
	view(): MarkingView;
}

/** Accumulate a `TokenBridgedOut` event into a bridged-out map. */
function applyBridgedOut(map: Map<string, Token[]>, ev: PersistedEvent['event']) {
	if (ev.type === 'TokenBridgedOut') {
		const arr = map.get(ev.source_place_id) ?? [];
		arr.push(ev.token);
		map.set(ev.source_place_id, arr);
	}
}

export function createMarkingBuffer(cap: number = DEFAULT_EVENT_BUFFER_CAP): MarkingBuffer {
	let events: PersistedEvent[] = [];
	let replayIndex = -1;
	let evictedCount = 0;
	let lastSequence = -1;
	// Current state after ALL events ever applied (incremental).
	const live = new Map<string, Token[]>();
	const liveBridged = new Map<string, Token[]>();
	// Folded state of the EVICTED prefix only.
	const base = new Map<string, Token[]>();
	const baseBridged = new Map<string, Token[]>();

	const followingLive = () => replayIndex >= events.length - 1;

	function trim() {
		const overflow = events.length - cap;
		if (overflow <= 0) return;
		for (const e of events.slice(0, overflow)) {
			applyEventToMarking(base, e.event);
			applyBridgedOut(baseBridged, e.event);
		}
		events = events.slice(overflow);
		evictedCount += overflow;
		// Keep the cursor on the same logical event in the shorter buffer (clamp
		// into the retained window if it fell below the new floor).
		replayIndex = Math.max(-1, replayIndex - overflow);
	}

	return {
		append(incoming) {
			if (!incoming.length) return false;
			const seen = new Set(events.map((e) => e.sequence));
			const unique = incoming.filter((e) => {
				if (seen.has(e.sequence)) return false;
				seen.add(e.sequence);
				return true;
			});
			if (!unique.length) return false;
			const wasAtEnd = followingLive();
			for (const e of unique) {
				applyEventToMarking(live, e.event);
				applyBridgedOut(liveBridged, e.event);
			}
			events = [...events, ...unique];
			trim();
			if (wasAtEnd) replayIndex = events.length - 1;
			lastSequence = events[events.length - 1].sequence;
			return true;
		},
		reset() {
			events = [];
			replayIndex = -1;
			evictedCount = 0;
			lastSequence = -1;
			live.clear();
			liveBridged.clear();
			base.clear();
			baseBridged.clear();
		},
		setReplayIndex(index) {
			replayIndex = index;
		},
		get events() {
			return events;
		},
		get replayIndex() {
			return replayIndex;
		},
		get evictedCount() {
			return evictedCount;
		},
		get lastSequence() {
			return lastSequence;
		},
		view() {
			if (followingLive()) {
				return { marking: new Map(live), bridgedOut: new Map(liveBridged) };
			}
			const marking = new Map<string, Token[]>();
			for (const [k, v] of base) marking.set(k, [...v]);
			const bridged = new Map<string, Token[]>();
			for (const [k, v] of baseBridged) bridged.set(k, [...v]);
			const end = Math.min(replayIndex + 1, events.length);
			for (let i = 0; i < end; i++) {
				applyEventToMarking(marking, events[i].event);
				applyBridgedOut(bridged, events[i].event);
			}
			return { marking, bridgedOut: bridged };
		}
	};
}
