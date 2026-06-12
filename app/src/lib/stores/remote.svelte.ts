/**
 * Shared remote-data primitives for route pages.
 *
 * Three factories cover the loading/error/data `$state` cluster that route
 * pages otherwise hand-roll around the `$lib/api/*` wrappers:
 *   createFetchState  — one resource fetched on demand (detail pages).
 *   createListState   — a filtered listing reloaded when the filter changes,
 *                       with the last filter retained for `refetch()`.
 *   createPolledState — a snapshot re-fetched on a fixed interval.
 *
 * House style (see tasks.svelte.ts): factory functions over `$state`,
 * returning a getter-object. Fetchers/listers/pollers THROW on failure — the
 * `$lib/api/*` wrappers already convert openapi-fetch's `{ data, error }`
 * results into thrown `ApiError`s — and the stores capture
 * `e instanceof Error ? e.message : <fallback>`. `data`/`items`/`error` are
 * also settable so pages keep their local mutations (optimistic updates,
 * action-failure messages) without a parallel state cluster.
 */

export interface RemoteStateOptions {
	/** Error message used when the thrown value is not an `Error`. */
	errorFallback?: string;
}

export interface ListStateOptions extends RemoteStateOptions {
	/** Blank the list when a load fails (some pages keep the stale items). */
	resetOnError?: boolean;
}

export interface PolledStateOptions extends RemoteStateOptions {
	/** Called on every failed poll (e.g. surface a toast). `error` is set regardless. */
	onError?: (e: unknown) => void;
}

const DEFAULT_ERROR = 'Failed to load';

function messageOf(e: unknown, fallback: string | undefined): string {
	return e instanceof Error ? e.message : (fallback ?? DEFAULT_ERROR);
}

/**
 * One remote resource, fetched on demand. `loading` starts true so the first
 * render shows the loading branch before `refetch()` has resolved; call
 * `refetch()` (typically from an `$effect` keyed on route params) to load.
 */
export function createFetchState<T>(fetcher: () => Promise<T>, opts: RemoteStateOptions = {}) {
	let data = $state<T | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	async function refetch(): Promise<void> {
		loading = true;
		error = null;
		try {
			data = await fetcher();
		} catch (e) {
			error = messageOf(e, opts.errorFallback);
		} finally {
			loading = false;
		}
	}

	return {
		get data() {
			return data;
		},
		set data(v: T | null) {
			data = v;
		},
		get loading() {
			return loading;
		},
		get error() {
			return error;
		},
		set error(v: string | null) {
			error = v;
		},
		refetch
	};
}

/**
 * A filtered listing. `load(filter)` retains the filter so `refetch()`
 * reloads the current view after a mutation. `loading` starts true (same
 * first-render contract as createFetchState).
 */
export function createListState<T, F = void>(
	lister: (filter: F) => Promise<T[]>,
	opts: ListStateOptions = {}
) {
	let items = $state<T[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let lastFilter = undefined as F;

	async function load(filter: F): Promise<void> {
		lastFilter = filter;
		loading = true;
		error = null;
		try {
			items = await lister(filter);
		} catch (e) {
			error = messageOf(e, opts.errorFallback);
			if (opts.resetOnError) items = [];
		} finally {
			loading = false;
		}
	}

	function refetch(): Promise<void> {
		return load(lastFilter);
	}

	return {
		get items() {
			return items;
		},
		set items(v: T[]) {
			items = v;
		},
		get loading() {
			return loading;
		},
		get error() {
			return error;
		},
		set error(v: string | null) {
			error = v;
		},
		load,
		refetch
	};
}

/**
 * A snapshot polled immediately and then every `intervalMs`. The last good
 * `data` is kept across transient failures (`error` flips instead);
 * `lastUpdated` records the last successful poll. `poll()` is exposed for
 * manual refreshes after mutations.
 *
 * Uses `$effect` for the interval lifecycle, so it MUST be constructed during
 * component initialisation (top level of a `<script>`); constructing it later
 * (event handler, async callback) throws `effect_orphan`. A call site that
 * needs to start polling outside component init should keep the manual
 * setInterval/clearInterval pattern rather than reach for `$effect.root`.
 */
export function createPolledState<T>(
	poller: () => Promise<T>,
	intervalMs = 5000,
	opts: PolledStateOptions = {}
) {
	let data = $state<T | null>(null);
	let error = $state<string | null>(null);
	let lastUpdated = $state<Date | null>(null);

	async function poll(): Promise<void> {
		try {
			data = await poller();
			lastUpdated = new Date();
			error = null;
		} catch (e) {
			error = messageOf(e, opts.errorFallback);
			opts.onError?.(e);
		}
	}

	$effect(() => {
		void poll();
		const t = setInterval(() => void poll(), intervalMs);
		return () => clearInterval(t);
	});

	return {
		get data() {
			return data;
		},
		get error() {
			return error;
		},
		set error(v: string | null) {
			error = v;
		},
		get lastUpdated() {
			return lastUpdated;
		},
		poll
	};
}
