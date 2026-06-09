import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

export default defineConfig({
	plugins: [svelte({ hot: false })],
	test: {
		include: ['src/**/*.test.ts'],
		environment: 'jsdom',
		globals: true,
		setupFiles: ['./vitest-setup.ts']
	},
	resolve: {
		// Resolve Svelte (and any browser-conditioned exports) to their client
		// build so component tests can actually `mount`/`render` under jsdom.
		// Without this, vite picks Svelte's server export and
		// `@testing-library/svelte`'s render throws `mount is not available on
		// the server`. Verified: the full existing unit suite still passes with
		// this condition set.
		conditions: ['browser'],
		alias: {
			$lib: new URL('./src/lib', import.meta.url).pathname,
			// SvelteKit's `$app/environment` is a virtual module the unit lane
			// (plain vitest, no SvelteKit plugin) can't resolve. Components
			// reachable from tests (e.g. output-renderers/index → JsonBlock →
			// CodeEditor) import `browser` from it, so point it at a stub.
			'$app/environment': new URL(
				'./src/lib/testing/app-environment-stub.ts',
				import.meta.url
			).pathname,
			// `$app/navigation` is likewise virtual; route pages reachable from
			// tests (e.g. the tasks inbox) call `goto`. Point it at a no-op stub
			// so the unit lane can import them; tests may still vi.mock it.
			'$app/navigation': new URL(
				'./src/lib/testing/app-navigation-stub.ts',
				import.meta.url
			).pathname
		}
	}
});
