import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
	resolve: {
		dedupe: ['svelte', 'bits-ui']
	},
	server: {
		proxy: {
			// Petri-lab engine API (port 3030)
			'/petri': {
				target: 'http://localhost:3030',
				changeOrigin: true,
				rewrite: (path: string) => path.replace(/^\/petri/, '')
			},
			// Yjs WebSocket — must come before generic /api so the upgrade is preserved
			'/api/yjs': {
				target: 'http://localhost:3100',
				ws: true,
				changeOrigin: true
			},
			// All other /api/* requests → mekhan-service
			'/api': {
				target: 'http://localhost:3100',
				changeOrigin: true
			}
		}
	}
});
