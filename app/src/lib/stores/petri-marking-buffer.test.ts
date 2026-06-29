import { describe, it, expect } from 'vitest';

import { createMarkingBuffer } from './petri-marking-buffer';
import { projectMarking } from './petri-projection';
import type { DomainEvent, PersistedEvent, Token } from '$lib/types/petri';

// ── fixtures ──────────────────────────────────────────────────────────────

function tok(id: string): Token {
	return { id, color: { type: 'Unit' }, created_at: '2026-01-01T00:00:00Z' };
}

function ev(sequence: number, event: DomainEvent): PersistedEvent {
	return { sequence, timestamp: '', event, hash: String(sequence), previous_hash: null };
}

function created(seq: number, place: string, tokenId: string): PersistedEvent {
	return ev(seq, { type: 'TokenCreated', token: tok(tokenId), place_id: place });
}

/** A TransitionFired that consumes `consumed` (place,tokenId) and produces
 *  `produced` (place,tokenId). */
function fired(
	seq: number,
	consumed: [string, string][],
	produced: [string, string][]
): PersistedEvent {
	return ev(seq, {
		type: 'TransitionFired',
		transition_id: 't',
		consumed_tokens: consumed,
		produced_tokens: produced.map(([p, id]) => [p, tok(id)] as [string, Token])
	});
}

function bridged(seq: number, place: string, tokenId: string): PersistedEvent {
	return ev(seq, {
		type: 'TokenBridgedOut',
		token: tok(tokenId),
		source_place_id: place,
		source_place_name: place,
		target_net_id: 'n',
		target_place_name: 'p',
		transition_id: 't',
		signal_key: 'k'
	});
}

/** Normalize a marking map to `{ place: sortedTokenIds }` for stable equality. */
function snap(m: Map<string, Token[]>): Record<string, string[]> {
	const out: Record<string, string[]> = {};
	for (const [place, tokens] of m) out[place] = tokens.map((t) => t.id).sort();
	return out;
}

/** The oracle: fold the FULL event list with the shared pure projector. */
function oracle(events: PersistedEvent[], upTo = events.length - 1): Record<string, string[]> {
	return snap(projectMarking(events, upTo));
}

// ── tests ─────────────────────────────────────────────────────────────────

describe('createMarkingBuffer', () => {
	it('incremental live marking equals a full fold (no eviction)', () => {
		const events = [
			created(0, 'a', 'x1'),
			created(1, 'a', 'x2'),
			fired(2, [['a', 'x1']], [['b', 'y1']]),
			created(3, 'b', 'y2')
		];
		const buf = createMarkingBuffer(100);
		buf.append(events);
		expect(snap(buf.view().marking)).toEqual(oracle(events));
		// a: x2 (x1 consumed), b: y1, y2
		expect(snap(buf.view().marking)).toEqual({ a: ['x2'], b: ['y1', 'y2'] });
	});

	it('appending one-at-a-time equals appending in a batch', () => {
		const events = Array.from({ length: 50 }, (_, i) => created(i, `p${i % 4}`, `t${i}`));
		const oneAtATime = createMarkingBuffer(100);
		for (const e of events) oneAtATime.append([e]);
		const batched = createMarkingBuffer(100);
		batched.append(events);
		expect(snap(oneAtATime.view().marking)).toEqual(snap(batched.view().marking));
		expect(snap(batched.view().marking)).toEqual(oracle(events));
	});

	it('dedupes by sequence and tracks the high-water cursor', () => {
		const buf = createMarkingBuffer(100);
		expect(buf.lastSequence).toBe(-1);
		expect(buf.append([created(0, 'a', 'x'), created(1, 'a', 'y')])).toBe(true);
		// Re-appending the same sequences is a no-op.
		expect(buf.append([created(0, 'a', 'x'), created(1, 'a', 'y')])).toBe(false);
		expect(buf.lastSequence).toBe(1);
		expect(snap(buf.view().marking)).toEqual({ a: ['x', 'y'] });
	});

	it('bounds the buffer to the cap while live marking stays correct', () => {
		const events = Array.from({ length: 100 }, (_, i) => created(i, 'sink', `t${i}`));
		const buf = createMarkingBuffer(10);
		buf.append(events);
		// Tail is capped; the rest was evicted into the base.
		expect(buf.events.length).toBe(10);
		expect(buf.evictedCount).toBe(90);
		// Yet the live marking reflects ALL 100 tokens (base ⊕ tail).
		expect(snap(buf.view().marking)).toEqual(oracle(events));
		expect(buf.view().marking.get('sink')!.length).toBe(100);
	});

	it('consume/produce survives eviction (base fold is correct)', () => {
		// Create 20 tokens at `a`, then consume them pairwise into `b`, with a
		// tiny cap so the creates are evicted before the consumes apply.
		const events: PersistedEvent[] = [];
		let seq = 0;
		for (let i = 0; i < 20; i++) events.push(created(seq++, 'a', `x${i}`));
		for (let i = 0; i < 20; i++) events.push(fired(seq++, [['a', `x${i}`]], [['b', `y${i}`]]));
		const buf = createMarkingBuffer(5);
		buf.append(events);
		// All `a` tokens consumed, 20 produced at `b` — even though the creating
		// events were evicted into the base long before the consumes ran.
		expect(snap(buf.view().marking)).toEqual(oracle(events));
		expect(buf.view().marking.has('a')).toBe(false);
		expect(buf.view().marking.get('b')!.length).toBe(20);
	});

	it('scrubbing back folds base ⊕ window to the right absolute event', () => {
		const events = Array.from({ length: 30 }, (_, i) => created(i, 'a', `t${i}`));
		const buf = createMarkingBuffer(10);
		buf.append(events);
		expect(buf.evictedCount).toBe(20); // events 0..19 evicted, 20..29 retained
		// Scrub to local index 3 → absolute event 20 + 3 = 23 → tokens t0..t23.
		buf.setReplayIndex(3);
		const view = buf.view();
		expect(snap(view.marking)).toEqual(oracle(events, buf.evictedCount + 3));
		expect(view.marking.get('a')!.length).toBe(24);
	});

	it('appends while scrubbed back do not yank the cursor to live', () => {
		const buf = createMarkingBuffer(100);
		buf.append(Array.from({ length: 10 }, (_, i) => created(i, 'a', `t${i}`)));
		buf.setReplayIndex(4); // scrubbed into history
		buf.append([created(10, 'a', 't10')]);
		// Cursor stays where the user parked it; the new event is buffered but the
		// view still reflects the scrubbed position.
		expect(buf.replayIndex).toBe(4);
		expect(buf.view().marking.get('a')!.length).toBe(5); // t0..t4
	});

	it('tracks bridged-out tokens (incrementally and through eviction)', () => {
		const events = [
			created(0, 'a', 'x'),
			bridged(1, 'a', 'x'),
			...Array.from({ length: 20 }, (_, i) => created(2 + i, 'filler', `f${i}`))
		];
		const buf = createMarkingBuffer(5);
		buf.append(events);
		// The bridged-out create+bridge pair was evicted, but the bridged-out
		// accumulator still carries it.
		const bridgedOut = buf.view().bridgedOut;
		expect(bridgedOut.get('a')?.map((t) => t.id)).toEqual(['x']);
		// And `a` is empty in the marking (the token left).
		expect(buf.view().marking.has('a')).toBe(false);
	});

	it('reset clears everything', () => {
		const buf = createMarkingBuffer(100);
		buf.append(Array.from({ length: 10 }, (_, i) => created(i, 'a', `t${i}`)));
		buf.reset();
		expect(buf.events.length).toBe(0);
		expect(buf.replayIndex).toBe(-1);
		expect(buf.evictedCount).toBe(0);
		expect(buf.lastSequence).toBe(-1);
		expect(buf.view().marking.size).toBe(0);
	});

	it('view() returns fresh Map refs (so reactive consumers re-derive)', () => {
		const buf = createMarkingBuffer(100);
		buf.append([created(0, 'a', 'x')]);
		expect(buf.view().marking).not.toBe(buf.view().marking);
	});
});
