/**
 * Shared on-graph live-feed slot cap — a module singleton that bounds how many
 * edge media widgets may hold an OPEN live tap at once.
 *
 * Each on-edge widget that wants to stream must first `request()` a slot; if one
 * is granted it opens its `liveTapRegistry` subscription, otherwise it renders a
 * passive "available" badge that the user can click to force a slot. Every
 * granted widget MUST `release()` exactly once when it stops (teardown,
 * off-viewport, channel closed). The cap exists because a single instance graph
 * can have many renderable edges and each open tap is a live network read +
 * decode pipeline; without a cap, scrolling a busy run would open dozens of
 * concurrent MSE/MJPEG decoders.
 *
 * Semantics:
 *  - At most `MAX_LIVE_FEEDS` slots are granted at once (FIFO is not modelled —
 *    grants are first-come; a denied widget simply shows the badge and may
 *    retry on user click or when another widget releases).
 *  - `request()` returns `true` if a slot was granted (the caller now owns it
 *    and must `release()`), `false` if at capacity.
 *  - `release()` is idempotent at the call-site's discretion: it just decrements
 *    while > 0. Callers guard with their own "do I hold a slot" flag.
 *  - `subscribe()` lets a widget reactively re-attempt when capacity frees up
 *    (a released slot notifies waiters so a badge can auto-upgrade to live).
 *
 * This is deliberately framework-light (no Svelte runes) so it can be unit
 * tested and shared across every widget instance regardless of component tree.
 */

/** Max simultaneously-open on-graph live taps. ~6 keeps a busy run responsive. */
export const MAX_LIVE_FEEDS = 6;

let used = 0;
const waiters = new Set<() => void>();

/** Try to claim a live-feed slot. Returns true if granted (caller must release). */
export function request(): boolean {
	if (used >= MAX_LIVE_FEEDS) return false;
	used += 1;
	return true;
}

/** Release a previously-granted slot. No-op below zero. Notifies one waiter. */
export function release(): void {
	if (used <= 0) return;
	used -= 1;
	// Wake waiters so a badge can auto-upgrade now that a slot opened up.
	for (const w of waiters) w();
}

/** Slots currently held (test/diagnostic). */
export function inUse(): number {
	return used;
}

/** True when no slot is free. */
export function atCapacity(): boolean {
	return used >= MAX_LIVE_FEEDS;
}

/**
 * Register a callback fired whenever a slot is released (capacity may have
 * freed up). Returns an unsubscribe fn. Used by widgets sitting on a badge to
 * retry their `request()` without polling.
 */
export function subscribe(onFreed: () => void): () => void {
	waiters.add(onFreed);
	return () => waiters.delete(onFreed);
}

/** Test-only: reset the singleton between tests. */
export function _reset(): void {
	used = 0;
	waiters.clear();
}
