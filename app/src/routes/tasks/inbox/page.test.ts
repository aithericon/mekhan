/**
 * Regression test for the inbox SESSION liveness source.
 *
 * `session`-mode human availability (the DEFAULT enroll mode) survives only
 * while the browser holds `/api/v1/tasks/stream` open: the server publishes the
 * `human.{member}.presence` heartbeat on connect + every 10s ping, and the
 * presence controller renews the member's 45s TTL off it. The availability
 * toggle lives on THIS page, so THIS page must hold that stream — otherwise a
 * member who toggles available is admitted and then silently reaped by the TTL
 * sweep ~45s later. That wiring was missing once; this locks it in:
 *   - mounting the inbox opens an SSE to `/api/v1/tasks/stream`,
 *   - unmounting closes it (no leaked heartbeat for a backgrounded member).
 */
import { describe, it, expect, vi, afterEach, beforeEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';

// Capture connectSse calls + hand back a closable connection handle. Defined
// via vi.hoisted so the spies exist when the hoisted vi.mock factory runs.
const { connectSse, close } = vi.hoisted(() => {
	const close = vi.fn();
	const connectSse = vi.fn((_url: string, _opts: unknown) => ({ close }));
	return { connectSse, close };
});

vi.mock('$lib/net/sse', () => ({ connectSse }));
vi.mock('$lib/auth/fetch', () => ({ authFetch: vi.fn() }));
vi.mock('$lib/api/client', () => ({
	listTaskInbox: vi.fn(async () => ({ tasks: [] })),
	claimTask: vi.fn()
}));
vi.mock('$lib/api/roster', () => ({
	getMyEnrollments: vi.fn(async () => []),
	setAvailability: vi.fn()
}));
vi.mock('$lib/api/capacities', () => ({
	listCapacities: vi.fn(async () => [])
}));

import InboxPage from './+page.svelte';

beforeEach(() => {
	connectSse.mockClear();
	close.mockClear();
});
afterEach(() => cleanup());

async function flush() {
	await Promise.resolve();
	await Promise.resolve();
}

describe('inbox session liveness', () => {
	it('opens the task stream on mount (the session heartbeat source)', async () => {
		render(InboxPage);
		await flush();

		expect(connectSse).toHaveBeenCalledTimes(1);
		expect(connectSse).toHaveBeenCalledWith('/api/v1/tasks/stream', expect.anything());
	});

	it('closes the stream on unmount (no leaked heartbeat)', async () => {
		const { unmount } = render(InboxPage);
		await flush();
		expect(close).not.toHaveBeenCalled();

		unmount();
		expect(close).toHaveBeenCalledTimes(1);
	});
});
