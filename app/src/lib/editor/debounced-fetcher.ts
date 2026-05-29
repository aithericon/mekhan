/**
 * Shared debounce + sequence-guard helper for async derive effects.
 *
 * Both AutomatedStepSection (derives the output Port from backend config)
 * and SubWorkflowSection (derives the io-contract from the child template)
 * use the same structural pattern:
 *
 *   - A mutable timer handle and sequence counter kept in module scope.
 *   - On each reactive invalidation: cancel the previous timer, bump the
 *     sequence, schedule a new timer that fires the fetch, and checks the
 *     sequence before applying the result.
 *
 * `createDebouncedFetcher` encapsulates the timer + sequence state and
 * exposes a single `schedule(fn, delayMs?)` method. Call sites provide the
 * actual fetch + application logic as an async callback; the helper ensures
 * only the last-scheduled call's result is applied.
 *
 * Usage inside a $effect:
 *
 *   const fetcher = createDebouncedFetcher();
 *   $effect(() => {
 *     const dep = someReactiveDep;
 *     fetcher.schedule(async (fresh) => {
 *       const result = await someApi(dep);
 *       if (!fresh()) return;        // stale — another call was scheduled
 *       untrack(() => applyResult(result));
 *     });
 *   });
 *
 * `fresh()` returns `true` when the callback's sequence still matches the
 * latest — equivalent to the manual `seq !== deriveSeq` guard.
 */
export function createDebouncedFetcher(defaultDelay = 250) {
	let timer: ReturnType<typeof setTimeout> | null = null;
	let seq = 0;

	/**
	 * Cancel any pending timer, then schedule `fn` to run after `delay` ms.
	 * `fn` receives a `fresh` predicate: call it to check whether this
	 * invocation is still the latest before mutating reactive state.
	 */
	function schedule(fn: (fresh: () => boolean) => Promise<void>, delay = defaultDelay): void {
		if (timer !== null) clearTimeout(timer);
		const mySeq = ++seq;
		timer = setTimeout(() => {
			void fn(() => mySeq === seq);
		}, delay);
	}

	return { schedule };
}
