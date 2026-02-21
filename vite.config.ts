import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
	server: {
		proxy: {
			'/api/yjs': {
				target: 'http://localhost:3100',
				ws: true,
				changeOrigin: true
			},
			// File upload/serve — bypass SvelteKit to avoid body size limits
			'/api/files': {
				target: 'http://localhost:3100',
				changeOrigin: true
			}
		}
	}
});
