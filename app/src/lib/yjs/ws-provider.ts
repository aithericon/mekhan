/**
 * Custom WebSocket provider for the mekhan-service Yjs sync protocol.
 *
 * The mekhan backend uses a simple binary protocol:
 *   [0, ...state_vector] = SyncStep1 (client sends its state vector)
 *   [1, ...update]       = SyncStep2 (server sends missing updates)
 *   [2, ...update]       = SyncUpdate (incremental update, broadcast to others)
 *
 * This differs from y-websocket's y-protocols format which wraps each message
 * in an additional VarUint framing layer.
 */

import * as Y from 'yjs';
import { Awareness } from 'y-protocols/awareness';

const MSG_SYNC_STEP1 = 0;
const MSG_SYNC_STEP2 = 1;
const MSG_SYNC_UPDATE = 2;

const RECONNECT_BASE_MS = 500;
const RECONNECT_MAX_MS = 10_000;

type StatusEvent = { status: 'connecting' | 'connected' | 'disconnected' };
type StatusHandler = (event: StatusEvent) => void;
type SyncHandler = (synced: boolean) => void;

export class MekhanWsProvider {
	doc: Y.Doc;
	awareness: Awareness;

	private wsUrl: string;
	private ws: WebSocket | null = null;
	private listeners = new Map<string, Set<StatusHandler>>();
	private syncHandlers = new Set<SyncHandler>();
	private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	private reconnectDelay = RECONNECT_BASE_MS;
	private shouldConnect = true;
	private synced = false;
	private currentStatus: StatusEvent['status'] = 'connecting';

	constructor(wsUrl: string, templateId: string, doc: Y.Doc, token?: string) {
		this.doc = doc;
		const base = `${wsUrl}/${templateId}`;
		// Auth: browsers can't send Authorization on WS upgrades, so the
		// access token rides as a query param. The backend validates it inside
		// the upgrade handler against the same `TokenVerifier`.
		this.wsUrl = token ? `${base}?token=${encodeURIComponent(token)}` : base;
		this.awareness = new Awareness(doc);

		// Listen for local doc changes and send them as SyncUpdate
		this.doc.on('update', this.handleDocUpdate);

		this.connect();
	}

	private handleDocUpdate = (update: Uint8Array, origin: unknown) => {
		// Don't echo back updates that came from the server
		if (origin === this) return;
		this.sendMessage(MSG_SYNC_UPDATE, update);
	};

	private connect() {
		if (!this.shouldConnect) return;
		this.setStatus('connecting');

		try {
			this.ws = new WebSocket(this.wsUrl);
			this.ws.binaryType = 'arraybuffer';
		} catch {
			this.scheduleReconnect();
			return;
		}

		this.ws.onopen = () => {
			this.reconnectDelay = RECONNECT_BASE_MS;
			this.setStatus('connected');

			// Send SyncStep1: our state vector so the server can diff
			const sv = Y.encodeStateVector(this.doc);
			this.sendMessage(MSG_SYNC_STEP1, sv);
		};

		this.ws.onmessage = (event: MessageEvent) => {
			const data = new Uint8Array(event.data as ArrayBuffer);
			if (data.length < 1) return;

			const msgType = data[0];
			const payload = data.slice(1);

			switch (msgType) {
				case MSG_SYNC_STEP2:
					// Server sends missing updates — apply them
					Y.applyUpdate(this.doc, payload, this);
					this.setSynced(true);
					break;
				case MSG_SYNC_UPDATE:
					// Broadcast update from another client — apply it
					Y.applyUpdate(this.doc, payload, this);
					break;
				default:
					console.warn('Unknown Yjs message type:', msgType);
			}
		};

		this.ws.onclose = () => {
			this.ws = null;
			this.setSynced(false);
			this.setStatus('disconnected');
			this.scheduleReconnect();
		};

		this.ws.onerror = () => {
			// onclose will fire after onerror
		};
	}

	private sendMessage(type: number, payload: Uint8Array) {
		if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;

		const msg = new Uint8Array(1 + payload.length);
		msg[0] = type;
		msg.set(payload, 1);
		this.ws.send(msg);
	}

	private scheduleReconnect() {
		if (!this.shouldConnect) return;
		if (this.reconnectTimer) return;

		this.reconnectTimer = setTimeout(() => {
			this.reconnectTimer = null;
			this.reconnectDelay = Math.min(this.reconnectDelay * 1.5, RECONNECT_MAX_MS);
			this.connect();
		}, this.reconnectDelay);
	}

	private setStatus(status: StatusEvent['status']) {
		this.currentStatus = status;
		this.emit('status', { status });
	}

	on(event: string, handler: StatusHandler) {
		if (!this.listeners.has(event)) {
			this.listeners.set(event, new Set());
		}
		this.listeners.get(event)!.add(handler);

		// Replay current status to newly registered listeners so they
		// don't miss events that fired before they subscribed.
		if (event === 'status') {
			handler({ status: this.currentStatus });
		}
	}

	off(event: string, handler: StatusHandler) {
		this.listeners.get(event)?.delete(handler);
	}

	private emit(event: string, data: StatusEvent) {
		this.listeners.get(event)?.forEach((handler) => handler(data));
	}

	/**
	 * `true` once the server's authoritative document state has been applied
	 * (initial SyncStep2). Resets to `false` on disconnect. Collaborative
	 * editors must not bind to a Y.Text before this — binding to a
	 * not-yet-synced (empty/partial) shared text makes y-codemirror mirror the
	 * local initial content back into the doc as a fresh insert, concatenating
	 * duplicates into the persisted text.
	 */
	get isSynced(): boolean {
		return this.synced;
	}

	private setSynced(value: boolean) {
		if (this.synced === value) return;
		this.synced = value;
		this.syncHandlers.forEach((handler) => handler(value));
	}

	onSync(handler: SyncHandler) {
		this.syncHandlers.add(handler);
		// Replay current value so late subscribers don't miss the transition.
		handler(this.synced);
	}

	offSync(handler: SyncHandler) {
		this.syncHandlers.delete(handler);
	}

	disconnect() {
		this.shouldConnect = false;
		if (this.reconnectTimer) {
			clearTimeout(this.reconnectTimer);
			this.reconnectTimer = null;
		}
		if (this.ws) {
			this.ws.close();
			this.ws = null;
		}
	}

	destroy() {
		this.disconnect();
		this.doc.off('update', this.handleDocUpdate);
		this.awareness.destroy();
		this.listeners.clear();
		this.syncHandlers.clear();
	}
}
