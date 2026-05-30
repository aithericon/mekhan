// See https://svelte.dev/docs/kit/types#app.d.ts
// for information about these interfaces
declare global {
	namespace App {
		// interface Error {}
		// interface Locals {}
		// interface PageData {}
		// interface PageState {}
		// interface Platform {}
	}

	// Injected by vite.config.ts `define` from MEKHAN_SERVICE_URL: the dev-only
	// `ws://host:port` base the editor's Yjs WebSocket connects to directly.
	// See app/src/lib/yjs/session.ts for why the dev WS bypasses the Vite proxy.
	const __MEKHAN_WS_URL__: string;
}

export {};
