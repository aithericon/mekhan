/**
 * Tests for the global session-presence heartbeat.
 *
 * What this locks in:
 *   - an enrolled human pings immediately, then on the interval, until stopped,
 *   - the stop function halts the pings (no leak after sign-out / teardown),
 *   - a non-human session (no enrollments) never pings.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

const { getMyEnrollments, sendPresenceHeartbeat } = vi.hoisted(() => ({
	getMyEnrollments: vi.fn(),
	sendPresenceHeartbeat: vi.fn()
}));
vi.mock('$lib/api/roster', () => ({ getMyEnrollments, sendPresenceHeartbeat }));

import { startPresenceHeartbeat } from './heartbeat';

beforeEach(() => {
	vi.clearAllMocks();
	vi.useFakeTimers();
});
afterEach(() => {
	vi.useRealTimers();
});

describe('startPresenceHeartbeat', () => {
	it('pings immediately and on the interval for an enrolled human, then stops', async () => {
		getMyEnrollments.mockResolvedValue([{ capacity_id: 'c1' }]);
		const stop = startPresenceHeartbeat();

		// Flush the getMyEnrollments microtask → immediate ping + interval armed.
		await vi.advanceTimersByTimeAsync(0);
		expect(sendPresenceHeartbeat).toHaveBeenCalledTimes(1);

		await vi.advanceTimersByTimeAsync(10_000);
		expect(sendPresenceHeartbeat).toHaveBeenCalledTimes(2);
		await vi.advanceTimersByTimeAsync(10_000);
		expect(sendPresenceHeartbeat).toHaveBeenCalledTimes(3);

		stop();
		await vi.advanceTimersByTimeAsync(60_000);
		expect(sendPresenceHeartbeat).toHaveBeenCalledTimes(3); // no further pings
	});

	it('never pings for a non-human session (no enrollments)', async () => {
		getMyEnrollments.mockResolvedValue([]);
		const stop = startPresenceHeartbeat();

		await vi.advanceTimersByTimeAsync(60_000);
		expect(sendPresenceHeartbeat).not.toHaveBeenCalled();
		stop();
	});

	it('stops cleanly even if stopped before enrollments resolve', async () => {
		let resolve!: (v: unknown[]) => void;
		getMyEnrollments.mockReturnValue(new Promise((r) => (resolve = r)));
		const stop = startPresenceHeartbeat();
		stop(); // tear down before the enrollment fetch settles
		resolve([{ capacity_id: 'c1' }]);

		await vi.advanceTimersByTimeAsync(60_000);
		expect(sendPresenceHeartbeat).not.toHaveBeenCalled();
	});
});
