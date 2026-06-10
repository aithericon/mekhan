/**
 * Vitest stub for SvelteKit's virtual `$app/state` module.
 *
 * Shell components reachable from unit-tested route pages (e.g. the tasks
 * inbox page → `shell/PageTabs.svelte`) read `page.url.pathname` to mark the
 * active tab. The virtual module can't resolve under plain vitest (no
 * SvelteKit Vite plugin in the unit lane), so alias it to this stub in
 * `vitest.config.ts`. The object is intentionally inert (no rune reactivity);
 * tests that need a specific URL can still `vi.mock('$app/state', …)`.
 */
export const page = {
	url: new URL('http://localhost/'),
	params: {} as Record<string, string>,
	route: { id: null as string | null },
	status: 200,
	error: null as Error | null,
	data: {} as Record<string, unknown>,
	form: null as unknown,
	state: {} as Record<string, unknown>
};

export const navigating = {
	from: null,
	to: null,
	type: null,
	willUnload: null,
	delta: null,
	complete: null
};

export const updated = {
	current: false,
	check: async () => false
};
