/**
 * Vitest stub for SvelteKit's virtual `$app/navigation` module.
 *
 * Route `+page.svelte` components imported into the unit lane (e.g. the tasks
 * inbox page) call `goto(...)`. The virtual module can't resolve under plain
 * vitest (no SvelteKit Vite plugin), so alias it to this no-op stub in
 * `vitest.config.ts`. Tests that need to assert on navigation can still
 * `vi.mock('$app/navigation', …)` to override these.
 */
export const goto = async () => {};
export const invalidate = async () => {};
export const invalidateAll = async () => {};
export const beforeNavigate = () => {};
export const afterNavigate = () => {};
export const preloadData = async () => {};
export const preloadCode = async () => {};
export const pushState = () => {};
export const replaceState = () => {};
