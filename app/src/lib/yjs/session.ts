import * as Y from 'yjs';
import { MekhanWsProvider } from './ws-provider';

export type YjsSession = {
	doc: Y.Doc;
	provider: MekhanWsProvider;
	awareness: MekhanWsProvider['awareness'];
	destroy: () => void;
};

export function createYjsSession(templateId: string): YjsSession {
	const doc = new Y.Doc();

	// In dev, connect the WS DIRECTLY to the backend (not through the Vite dev
	// origin) to avoid both the SvelteKit catch-all route intercepting the
	// upgrade AND the Vite WS proxy. The backend base is injected at config
	// time from `MEKHAN_SERVICE_URL` (`__MEKHAN_WS_URL__`), which makes it
	// slot-aware: every worktree's dev stack lives on a different port block
	// (slot N → app `20000+N*100+3`, mekhan `…+0`), so a hardcoded `:13100`
	// would hit slot 0's backend — which doesn't have this template. In
	// production, same host (a reverse proxy handles the WS upgrade).
	//
	// Why we rewrite `localhost` → `127.0.0.1` in dev:
	// mekhan binds `0.0.0.0` (IPv4). On macOS `localhost` resolves to `::1`
	// first, so the WS tries IPv6, gets ECONNREFUSED, and waits ~10s before
	// retrying IPv4 — the long "Reconnecting…" seen in browser traces. (And
	// going via the Vite origin instead is worse: Vite binds IPv6 `::1`, so a
	// `127.0.0.1` connection to it is refused outright.) Forcing the literal
	// IPv4 address against mekhan skips DNS and the handshake completes in ~10ms.
	let wsUrl: string;
	if (typeof window !== 'undefined') {
		if (import.meta.env.DEV) {
			wsUrl = `${__MEKHAN_WS_URL__.replace('//localhost', '//127.0.0.1')}/api/yjs`;
		} else {
			const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
			wsUrl = `${proto}//${window.location.host}/api/yjs`;
		}
	} else {
		wsUrl = 'ws://127.0.0.1/api/yjs';
	}

	// BFF model: no token in the URL. Same-origin (prod) the `mekhan_session`
	// HttpOnly cookie rides the WS upgrade automatically; in dev the WS hits
	// the backend directly under dev_noop, which authenticates every request.
	const provider = new MekhanWsProvider(wsUrl, templateId, doc);
	const awareness = provider.awareness;

	// Expose Y.Doc for E2E test assertions (dev only, tree-shaken in prod)
	if (import.meta.env.DEV && typeof window !== 'undefined') {
		(window as any).__yjs_doc = doc;
	}

	function destroy() {
		provider.destroy();
		doc.destroy();
	}

	return { doc, provider, awareness, destroy };
}
