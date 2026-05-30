import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// Per-worktree dev isolation: the dev server port and the mekhan proxy target
// are read from the env exported by just/scripts/dev-ports.sh (via direnv /
// the `just dev up-app` recipe), falling back to the historical fixed values
// when unset. Lets concurrent worktrees run isolated stacks. Build/preview are
// unaffected — these only touch the dev `server` block.
const appPort = Number(process.env.MEKHAN_APP_PORT) || 15173;
const mekhanUrl = process.env.MEKHAN_SERVICE_URL || 'http://localhost:13100';
// `ws://host:port` base for the editor's Yjs WebSocket. In dev it connects
// DIRECTLY to this per-worktree mekhan backend (which binds 0.0.0.0, so the
// IPv4 `127.0.0.1` rewrite in session.ts is valid and fast), bypassing the
// Vite dev server's WS proxy — Vite binds IPv6 `::1`, so a proxied
// `ws://127.0.0.1:<appport>` would be refused. Only referenced in the
// `import.meta.env.DEV` branch (dead-code-eliminated in prod builds).
const mekhanWsUrl = mekhanUrl.replace(/^http/, 'ws');

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
	define: {
		__MEKHAN_WS_URL__: JSON.stringify(mekhanWsUrl)
	},
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
		port: appPort,
		proxy: {
			// Yjs WebSocket — must come before generic /api so the upgrade is preserved
			'/api/yjs': {
				target: mekhanUrl,
				ws: true,
				changeOrigin: true
			},
			// All /api/* requests → mekhan-service (includes /api/v1/* JSON API,
			// /api/auth/* OAuth bootstrap, /api/triggers/webhook/*).
			'/api': {
				target: mekhanUrl,
				changeOrigin: true
			},
			// Engine traffic also flows through mekhan in dev for parity with
			// prod single-origin serving. mekhan's `/petri/*` proxy strips the
			// prefix and forwards to `config.petri_lab_url` (default :3030).
			'/petri': {
				target: mekhanUrl,
				changeOrigin: true
			}
		}
	}
});
