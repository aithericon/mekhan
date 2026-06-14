import { describe, it, expect, vi, beforeEach } from 'vitest';

// The live store opens fetch + SSE on init(); stub it so the context's
// memoization/teardown logic is testable without a network or runtime.
const created: { init: ReturnType<typeof vi.fn>; destroy: ReturnType<typeof vi.fn> }[] = [];
vi.mock('$lib/stores/process-live.svelte', () => ({
	createProcessLiveStore: vi.fn(() => {
		const store = { init: vi.fn(), destroy: vi.fn() };
		created.push(store);
		return store;
	})
}));

import { createEmbedContext } from './embed-context';
import { createProcessLiveStore } from '$lib/stores/process-live.svelte';

describe('createEmbedContext', () => {
	beforeEach(() => {
		created.length = 0;
		vi.clearAllMocks();
	});

	it('reads processes live from the thunk', () => {
		let procs = [{ id: 'a', name: 'A' }];
		const { context } = createEmbedContext(() => procs);
		expect(context.processes).toEqual([{ id: 'a', name: 'A' }]);
		procs = [
			{ id: 'a', name: 'A' },
			{ id: 'b', name: 'B' }
		];
		expect(context.processes).toHaveLength(2);
	});

	it('memoizes one store per process and inits it exactly once', () => {
		const { context } = createEmbedContext(() => []);
		const s1 = context.getArtifactStore('p1');
		const s2 = context.getArtifactStore('p1');
		expect(s1).toBe(s2);
		expect(createProcessLiveStore).toHaveBeenCalledTimes(1);
		expect(s1.init).toHaveBeenCalledTimes(1);

		const s3 = context.getArtifactStore('p2');
		expect(s3).not.toBe(s1);
		expect(createProcessLiveStore).toHaveBeenCalledTimes(2);
	});

	it('destroy tears down every created store and clears the map', () => {
		const { context, destroy } = createEmbedContext(() => []);
		const a = context.getArtifactStore('p1');
		const b = context.getArtifactStore('p2');
		destroy();
		expect(a.destroy).toHaveBeenCalledTimes(1);
		expect(b.destroy).toHaveBeenCalledTimes(1);

		// Map cleared → a fresh store is minted on the next request.
		context.getArtifactStore('p1');
		expect(createProcessLiveStore).toHaveBeenCalledTimes(3);
	});
});
