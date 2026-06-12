import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
	isGraphStructuralEvent,
	GRAPH_NOISE_EVENTS,
	RefreshScheduler
} from './instance-graph-refresh';

describe('isGraphStructuralEvent', () => {
	it('treats engine domain events as structural', () => {
		for (const e of [
			'NetInitialized',
			'TransitionFired',
			'TokenBridgedOut',
			'ChannelOpened',
			'ChannelClosed',
			'ArtifactCreated',
			'LeaseAcquired',
			'NetCompleted',
			'NetCancelled'
		]) {
			expect(isGraphStructuralEvent(e)).toBe(true);
		}
	});

	it('excludes the high-frequency per-frame noise events', () => {
		expect(isGraphStructuralEvent('TokenCreated')).toBe(false);
		expect(isGraphStructuralEvent('EffectCompleted')).toBe(false);
		// And the exported set is exactly those two (kept in lock-step with the
		// layout's HEADER_NOISE_EVENTS).
		expect([...GRAPH_NOISE_EVENTS].sort()).toEqual(['EffectCompleted', 'TokenCreated']);
	});

	it('excludes the stream control frames', () => {
		expect(isGraphStructuralEvent('connected')).toBe(false);
		expect(isGraphStructuralEvent('result')).toBe(false);
	});
});

describe('RefreshScheduler', () => {
	beforeEach(() => vi.useFakeTimers());
	afterEach(() => vi.useRealTimers());

	it('coalesces a burst of notifies into a single debounced refetch', () => {
		const run = vi.fn();
		const s = new RefreshScheduler(run, { debounceMs: 300, followUpMs: 1000 });

		s.notify();
		s.notify();
		s.notify();
		expect(run).not.toHaveBeenCalled();

		vi.advanceTimersByTime(300);
		expect(run).toHaveBeenCalledTimes(1); // one refetch for the whole burst
	});

	it('fires one follow-up refetch after the debounced one to catch the lagging projection row', () => {
		const run = vi.fn();
		const s = new RefreshScheduler(run, { debounceMs: 300, followUpMs: 1000 });

		s.notify();
		vi.advanceTimersByTime(300);
		expect(run).toHaveBeenCalledTimes(1);

		vi.advanceTimersByTime(1000);
		expect(run).toHaveBeenCalledTimes(2); // debounced + follow-up
	});

	it('does not reset the debounce timer on every notify (bounded cadence under a steady stream)', () => {
		const run = vi.fn();
		const s = new RefreshScheduler(run, { debounceMs: 300, followUpMs: 1000 });

		s.notify();
		vi.advanceTimersByTime(200);
		s.notify(); // mid-window — must NOT push the fire out
		vi.advanceTimersByTime(100);
		expect(run).toHaveBeenCalledTimes(1); // fired at the original +300, not +400
	});

	it('starts a fresh burst after the previous debounced fire', () => {
		const run = vi.fn();
		const s = new RefreshScheduler(run, { debounceMs: 300, followUpMs: 1000 });

		s.notify();
		vi.advanceTimersByTime(300);
		expect(run).toHaveBeenCalledTimes(1);

		// A new event after the fire schedules a fresh debounced refetch.
		s.notify();
		vi.advanceTimersByTime(300);
		expect(run).toHaveBeenCalledTimes(2);
	});

	it('dispose cancels all pending timers (no late refetch after teardown)', () => {
		const run = vi.fn();
		const s = new RefreshScheduler(run, { debounceMs: 300, followUpMs: 1000 });

		s.notify();
		vi.advanceTimersByTime(300);
		expect(run).toHaveBeenCalledTimes(1); // debounced fired, follow-up pending

		s.dispose();
		vi.advanceTimersByTime(5000);
		expect(run).toHaveBeenCalledTimes(1); // follow-up was cancelled
	});

	it('dispose before the debounce fires cancels the refetch entirely', () => {
		const run = vi.fn();
		const s = new RefreshScheduler(run, { debounceMs: 300, followUpMs: 1000 });

		s.notify();
		s.dispose();
		vi.advanceTimersByTime(5000);
		expect(run).not.toHaveBeenCalled();
	});
});
