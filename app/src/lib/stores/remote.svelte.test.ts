import { describe, it, expect, vi, afterEach } from 'vitest';
import { flushSync } from 'svelte';
import { createFetchState, createListState, createPolledState } from './remote.svelte';

// Microtask drain (NOT timer-based — works under fake timers too).
const flush = () => new Promise<void>((r) => queueMicrotask(() => queueMicrotask(r)));

describe('createFetchState', () => {
	it('starts loading and resolves data', async () => {
		const fetcher = vi.fn(async () => ({ id: 'a' }));
		const s = createFetchState(fetcher);
		expect(s.loading).toBe(true);
		expect(s.data).toBeNull();

		await s.refetch();
		expect(s.loading).toBe(false);
		expect(s.data).toEqual({ id: 'a' });
		expect(s.error).toBeNull();
	});

	it('captures Error messages and keeps prior data', async () => {
		let fail = false;
		const s = createFetchState(async () => {
			if (fail) throw new Error('boom');
			return 1;
		});
		await s.refetch();
		fail = true;
		await s.refetch();
		expect(s.error).toBe('boom');
		expect(s.data).toBe(1); // last good value retained
		expect(s.loading).toBe(false);
	});

	it('falls back for non-Error throws (custom + default)', async () => {
		const custom = createFetchState(
			async () => {
				throw 'nope';
			},
			{ errorFallback: 'Failed to load task' }
		);
		await custom.refetch();
		expect(custom.error).toBe('Failed to load task');

		const dflt = createFetchState(async () => {
			throw 'nope';
		});
		await dflt.refetch();
		expect(dflt.error).toBe('Failed to load');
	});

	it('refetch clears a previous error and loading is true in flight', async () => {
		let fail = true;
		let resolve!: (v: number) => void;
		const s = createFetchState(() => {
			if (fail) return Promise.reject(new Error('boom'));
			return new Promise<number>((r) => (resolve = r));
		});
		await s.refetch();
		expect(s.error).toBe('boom');

		fail = false;
		const p = s.refetch();
		expect(s.error).toBeNull();
		expect(s.loading).toBe(true);
		resolve(7);
		await p;
		expect(s.data).toBe(7);
	});

	it('data and error are settable for page-local mutations', async () => {
		const s = createFetchState(async () => 1);
		await s.refetch();
		s.data = 2;
		s.error = 'action failed';
		expect(s.data).toBe(2);
		expect(s.error).toBe('action failed');
	});
});

describe('createListState', () => {
	it('loads items for a filter and flips loading', async () => {
		const lister = vi.fn(async (f: { q: string }) => [f.q]);
		const s = createListState(lister);
		expect(s.loading).toBe(true);
		expect(s.items).toEqual([]);

		await s.load({ q: 'x' });
		expect(s.items).toEqual(['x']);
		expect(s.loading).toBe(false);
		expect(lister).toHaveBeenCalledWith({ q: 'x' });
	});

	it('refetch reuses the last filter', async () => {
		const lister = vi.fn(async (f: string) => [f]);
		const s = createListState(lister);
		await s.load('a');
		await s.refetch();
		expect(lister).toHaveBeenCalledTimes(2);
		expect(lister).toHaveBeenLastCalledWith('a');
	});

	it('keeps stale items on error by default, blanks with resetOnError', async () => {
		let fail = false;
		const lister = async () => {
			if (fail) throw new Error('down');
			return [1, 2];
		};
		const keep = createListState(lister);
		await keep.load();
		fail = true;
		await keep.refetch();
		expect(keep.error).toBe('down');
		expect(keep.items).toEqual([1, 2]);

		fail = false;
		const reset = createListState(lister, { resetOnError: true, errorFallback: 'nope' });
		await reset.load();
		fail = true;
		await reset.refetch();
		expect(reset.error).toBe('down');
		expect(reset.items).toEqual([]);
	});

	it('items are settable for optimistic updates', async () => {
		const s = createListState(async () => [{ id: 1, name: 'a' }]);
		await s.load();
		s.items = s.items.map((x) => ({ ...x, name: 'b' }));
		expect(s.items[0].name).toBe('b');
	});
});

describe('createPolledState', () => {
	afterEach(() => {
		vi.useRealTimers();
	});

	// The interval lives in an $effect, so construction needs a reactive
	// context — $effect.root here stands in for component init.
	function rooted<T>(make: () => T): { state: T; destroy: () => void } {
		let state!: T;
		const destroy = $effect.root(() => {
			state = make();
		});
		flushSync(); // run the interval-installing $effect
		return { state, destroy };
	}

	it('polls immediately, then on the interval; lastUpdated tracks success', async () => {
		vi.useFakeTimers();
		const poller = vi.fn(async () => 'snap');
		const { state, destroy } = rooted(() => createPolledState(poller, 5000));
		await flush();
		expect(poller).toHaveBeenCalledTimes(1);
		expect(state.data).toBe('snap');
		expect(state.lastUpdated).toBeInstanceOf(Date);

		await vi.advanceTimersByTimeAsync(5000);
		expect(poller).toHaveBeenCalledTimes(2);
		destroy();
	});

	it('keeps last data on failure, sets error + onError; recovery clears error', async () => {
		vi.useFakeTimers();
		let fail = false;
		const onError = vi.fn();
		const { state, destroy } = rooted(() =>
			createPolledState(
				async () => {
					if (fail) throw new Error('flaky');
					return 42;
				},
				5000,
				{ onError }
			)
		);
		await flush();
		expect(state.data).toBe(42);

		fail = true;
		await vi.advanceTimersByTimeAsync(5000);
		expect(state.error).toBe('flaky');
		expect(state.data).toBe(42); // stale-but-shown
		expect(onError).toHaveBeenCalledTimes(1);

		fail = false;
		await vi.advanceTimersByTimeAsync(5000);
		expect(state.error).toBeNull();
		destroy();
	});

	it('stops polling on teardown', async () => {
		vi.useFakeTimers();
		const poller = vi.fn(async () => 0);
		const { destroy } = rooted(() => createPolledState(poller, 5000));
		await flush();
		expect(poller).toHaveBeenCalledTimes(1);

		destroy();
		await vi.advanceTimersByTimeAsync(20000);
		expect(poller).toHaveBeenCalledTimes(1);
	});

	it('manual poll() refreshes outside the interval', async () => {
		vi.useFakeTimers();
		let n = 0;
		const { state, destroy } = rooted(() => createPolledState(async () => ++n, 5000));
		await flush();
		expect(state.data).toBe(1);

		await state.poll();
		expect(state.data).toBe(2);
		destroy();
	});
});
