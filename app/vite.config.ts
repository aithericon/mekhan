import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
	resolve: {
		dedupe: ['svelte', 'bits-ui']
	},
	// Pre-bundle the editor's heavy runtime deps so the first navigation to
	// /templates/[id] doesn't trigger lazy optimization mid-page-load. Without
	// these, Vite discovers `yjs`/`y-protocols`/`@xyflow/svelte` while serving
	// the editor route, optimizes them on the fly, and (depending on cache
	// state) can force a full reload — which is what the "Reconnecting..."
	// flash on first open was about.
	optimizeDeps: {
		include: [
			'@xyflow/svelte',
			'yjs',
			'y-protocols/awareness',
			'bits-ui',
			'@dagrejs/dagre'
		]
	},
	server: {
		proxy: {
			// Yjs WebSocket — must come before generic /api so the upgrade is preserved
			'/api/yjs': {
				target: 'http://localhost:3100',
				ws: true,
				changeOrigin: true
			},
			// All /api/* requests → mekhan-service (includes /api/v1/* JSON API,
			// /api/auth/* OAuth bootstrap, /api/triggers/webhook/*).
			'/api': {
				target: 'http://localhost:3100',
				changeOrigin: true
			},
			// Engine traffic also flows through mekhan in dev for parity with
			// prod single-origin serving. mekhan's `/petri/*` proxy strips the
			// prefix and forwards to `config.petri_lab_url` (default :3030).
			'/petri': {
				target: 'http://localhost:3100',
				changeOrigin: true
			}
		}
	}
});
