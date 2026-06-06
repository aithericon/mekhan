/**
 * Single-owner audio exclusivity store — only ONE edge feed may make sound at a
 * time across the whole instance graph.
 *
 * The instance/run canvas can show many audio-bearing edges at once (passive
 * PCM/MSE waveforms, plus videos whose tracks carry audio). They all draw their
 * waveforms silently in parallel, but only one may actually be *audible*. When
 * the user activates sound on an edge (unmutes a video, or hits "play" on an
 * audio waveform) that edge CLAIMS exclusivity: the previously-sounding owner is
 * stopped/muted via the callback it registered, and the new owner is recorded.
 *
 * This is a tiny rune-backed `$state` singleton so any component (an
 * `EdgeMediaWidget` instance, regardless of where it sits in the tree) can read
 * `currentOwner` reactively to show an "active" ring, and call `claim`/`release`
 * imperatively. Owner ids are the per-widget edge ids (unique per edge feed).
 *
 * Deliberately framework-light in shape (one module singleton) but uses Svelte 5
 * runes so `currentOwner` is reactive in components — hence the `.svelte.ts`
 * extension.
 */

/** The currently-sounding owner id, or `null` when nothing is audible. */
let owner = $state<string | null>(null);

/** The prior owner's stop/mute callback, invoked when a new owner claims. */
let stopOwner: (() => void) | null = null;

/** Read the current audio owner reactively. `null` ⇒ everything is silent. */
export function currentOwner(): string | null {
	return owner;
}

/** True when `id` currently owns audio (use for the "active" ring). */
export function isOwner(id: string): boolean {
	return owner === id;
}

/**
 * Claim audio for `id`. Stops/mutes whatever was sounding before (its registered
 * `stopPrev` callback fires) and records `id` as the new owner along with its own
 * `stopSelf` callback so a LATER claim can steal sound from it in turn.
 *
 * `stopSelf` is what the NEXT claimant calls to silence THIS owner — it should
 * pause/mute/teardown this widget's audible path (but may leave its passive
 * waveform running). Re-claiming while already the owner just swaps the callback.
 */
export function claim(id: string, stopSelf: () => void): void {
	if (owner !== null && owner !== id && stopOwner) {
		const prevStop = stopOwner;
		// Clear before invoking so a reentrant release() from the callback is a no-op.
		stopOwner = null;
		try {
			prevStop();
		} catch {
			/* a dead/torn-down owner can't object */
		}
	}
	owner = id;
	stopOwner = stopSelf;
}

/**
 * Release audio if `id` currently owns it (idempotent / ownership-checked). Does
 * NOT invoke the owner's stop callback — the owner is releasing voluntarily and
 * has already torn its own audio down. A release by a non-owner is a no-op.
 */
export function release(id: string): void {
	if (owner === id) {
		owner = null;
		stopOwner = null;
	}
}

/** Test-only: reset the singleton between tests. */
export function _reset(): void {
	owner = null;
	stopOwner = null;
}
