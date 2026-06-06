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
		alias: {
			$lib: new URL('./src/lib', import.meta.url).pathname,
			// SvelteKit's `$app/environment` is a virtual module the unit lane
			// (plain vitest, no SvelteKit plugin) can't resolve. Components
			// reachable from tests (e.g. output-renderers/index → JsonBlock →
			// CodeEditor) import `browser` from it, so point it at a stub.
			'$app/environment': new URL(
				'./src/lib/testing/app-environment-stub.ts',
				import.meta.url
			).pathname
		}
	}
});
