import * as Y from 'yjs';
import { auth } from '$lib/auth/store.svelte';
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
	let wsUrl: string;
	if (typeof window !== 'undefined') {
		const isDev = window.location.port === '5180' || import.meta.env.DEV;
		if (isDev) {
			wsUrl = `ws://${window.location.hostname}:3100/api/yjs`;
		} else {
			const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
			wsUrl = `${proto}//${window.location.host}/api/yjs`;
		}
	} else {
		wsUrl = 'ws://localhost:3100/api/yjs';
	}

	const provider = new MekhanWsProvider(wsUrl, templateId, doc, auth.getAccessToken());
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
