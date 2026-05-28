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

	// In dev, connect WS directly to the backend to avoid SvelteKit catch-all
	// route intercepting the WebSocket upgrade. In production, use the same
	// host (a reverse proxy will handle the WS upgrade).
	//
	// Why we rewrite `localhost` → `127.0.0.1` in dev:
	// mekhan-service binds `0.0.0.0:13100` (IPv4 only). On macOS `localhost`
	// resolves to `::1` first, so Firefox's WebSocket tries IPv6, gets
	// ECONNREFUSED, and waits ~10s before retrying IPv4 — that's the
	// long "Reconnecting…" we measured in the browser trace (10.66s between
	// `new WebSocket()` and `onopen` against `ws://localhost:13100/...`).
	// Forcing the literal IPv4 address skips DNS and the handshake completes
	// in ~10ms. (Chromium's WS happy-eyeballs is faster but not free either.)
	let wsUrl: string;
	if (typeof window !== 'undefined') {
		if (import.meta.env.DEV) {
			const host =
				window.location.hostname === 'localhost' ? '127.0.0.1' : window.location.hostname;
			wsUrl = `ws://${host}:13100/api/yjs`;
		} else {
			const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
			wsUrl = `${proto}//${window.location.host}/api/yjs`;
		}
	} else {
		wsUrl = 'ws://127.0.0.1:13100/api/yjs';
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
