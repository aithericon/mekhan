/**
 * Pure logic for the instance graph view's event-driven refresh.
 *
 * The instance layout already holds ONE SSE connection to
 * `GET /api/v1/instances/{id}/stream` (see `/instances/[id]/+layout.svelte`).
 * It surfaces each non-noise domain event as a reactive tick on the instance
 * context; `WorkflowGraphView` turns that tick into a coalesced refetch of the
 * async projection tables (step executions, marking, children, allocations).
 *
 * These tables are written by a SEPARATE causality consumer and LAG the raw
 * domain events by a beat or two, so an event means "a projection update is
 * imminent" — we SCHEDULE a (coalesced) refetch, never assume the row is
 * already there. This module owns the two pure pieces of that flow so they can
 * be unit-tested without a live backend or EventSource:
 *
 *   - `isGraphStructuralEvent(event)` — does this SSE event name warrant a
 *     graph refresh? (filters the high-frequency per-token/per-effect noise).
 *   - `RefreshScheduler` — debounce a burst of events into a single refetch,
 *     plus one short follow-up refetch so the just-arrived event's lagging
 *     projection row is reliably picked up.
 */

/**
 * High-frequency per-frame events that NEVER change the graph's projection
 * tables (a streaming workflow emits one of each per frame). They must not
 * trigger a refetch — otherwise the running phase floods the projection
 * endpoints. Mirrors the layout's `HEADER_NOISE_EVENTS` set; both gate on the
 * same names so the signal the layout bumps already excludes them.
 */
export const GRAPH_NOISE_EVENTS = new Set(['TokenCreated', 'EffectCompleted']);

/**
 * Non-event SSE control frames the stream emits that aren't domain events.
 * `connected` is the stream handshake; `result` is the terminal frame the
 * layout handles on its own (instance reload-until-terminal), so the graph
 * view's structural-event signal ignores it here — terminal catch-up in the
 * graph view is driven off the instance's `status` flipping, not this frame.
 */
const NON_STRUCTURAL_CONTROL = new Set(['connected', 'result']);

/**
 * Whether an SSE event name is a graph-relevant STRUCTURAL event — i.e. one
 * that may have moved the per-node runtime (a transition fired, a token
 * bridged, a lease acquired, a channel opened/closed, an artifact created, the
 * net initialized/completed/cancelled). Returns false for the control frames
 * and the per-frame noise events.
 */
export function isGraphStructuralEvent(event: string): boolean {
	if (NON_STRUCTURAL_CONTROL.has(event)) return false;
	if (GRAPH_NOISE_EVENTS.has(event)) return false;
	return true;
}

export type RefreshSchedulerOptions = {
	/** Coalesce a burst of events into one refetch this many ms after the
	 *  first event of the burst. */
	debounceMs?: number;
	/** Fire ONE more refetch this many ms after the debounced one, to catch the
	 *  lagging projection row the triggering event implied. */
	followUpMs?: number;
};

/**
 * Coalescing scheduler for projection refetches.
 *
 * `notify()` (called per structural event) schedules a debounced refetch; a
 * burst collapses into a single call. After that debounced refetch fires, one
 * follow-up refetch is scheduled `followUpMs` later to pick up the
 * just-arrived event's lagging projection row. A new `notify()` during the
 * debounce window does NOT reset the timer (leading-edge-anchored, trailing
 * fire) — so a steady stream still refetches at a bounded cadence rather than
 * being starved by continuous events.
 *
 * Timer-injectable so tests can drive it with fake timers deterministically.
 */
export class RefreshScheduler {
	private readonly run: () => void;
	private readonly debounceMs: number;
	private readonly followUpMs: number;
	private readonly setTimer: (fn: () => void, ms: number) => unknown;
	private readonly clearTimer: (h: unknown) => void;

	private debounceHandle: unknown = null;
	private followUpHandle: unknown = null;

	constructor(
		run: () => void,
		options: RefreshSchedulerOptions = {},
		timers: {
			setTimer?: (fn: () => void, ms: number) => unknown;
			clearTimer?: (h: unknown) => void;
		} = {}
	) {
		this.run = run;
		this.debounceMs = options.debounceMs ?? 300;
		this.followUpMs = options.followUpMs ?? 1000;
		this.setTimer =
			timers.setTimer ?? ((fn, ms) => setTimeout(fn, ms) as unknown);
		this.clearTimer =
			timers.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
	}

	/** Signal that a structural event arrived; schedules a coalesced refetch. */
	notify(): void {
		if (this.debounceHandle !== null) return; // already in a pending burst
		this.debounceHandle = this.setTimer(() => {
			this.debounceHandle = null;
			this.fire();
		}, this.debounceMs);
	}

	private fire(): void {
		this.run();
		// One trailing follow-up to catch the lagging projection row. Replace any
		// in-flight follow-up so back-to-back bursts don't stack them.
		if (this.followUpHandle !== null) this.clearTimer(this.followUpHandle);
		this.followUpHandle = this.setTimer(() => {
			this.followUpHandle = null;
			this.run();
		}, this.followUpMs);
	}

	/** Cancel all pending timers (component teardown / instance switch). */
	dispose(): void {
		if (this.debounceHandle !== null) {
			this.clearTimer(this.debounceHandle);
			this.debounceHandle = null;
		}
		if (this.followUpHandle !== null) {
			this.clearTimer(this.followUpHandle);
			this.followUpHandle = null;
		}
	}
}
