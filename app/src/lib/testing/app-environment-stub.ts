/**
 * Vitest stub for SvelteKit's virtual `$app/environment` module.
 *
 * Unit tests that import a `.svelte` component which (transitively) pulls in
 * `$app/environment` — e.g. any renderer reachable from
 * `output-renderers/index.ts` via `JsonBlock` → `CodeEditor` — would otherwise
 * fail to resolve the virtual module under plain vitest (no SvelteKit Vite
 * plugin in the unit lane). Aliased in `vitest.config.ts`.
 *
 * `browser` is the only export the codebase consumes; jsdom-backed tests run
 * "in a browser-like environment", so report `true`.
 */
export const browser = true;
export const dev = true;
export const building = false;
export const version = 'test';
