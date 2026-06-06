import { describe, it, expect, vi, beforeEach } from 'vitest';

// ── Mock livekit-client ──────────────────────────────────────────────────────
// Everything the hoisted `vi.mock` factory touches must itself be hoisted, so we
// build the fakes inside `vi.hoisted` and the factory just returns them. The
// `state` holder exposes the connect/disconnect spies + the last Room instance
// to the test body. A minimal fake Room records calls and lets the test drive
// `TrackSubscribed` to simulate a subscribed track.
const h = vi.hoisted(() => {
	type Handler = (...args: unknown[]) => void;
	const connect = vi.fn().mockResolvedValue(undefined);
	const disconnect = vi.fn().mockResolvedValue(undefined);
	const state: { lastRoom: FakeRoom | null } = { lastRoom: null };

	class FakeRoom {
		handlers: Record<string, Handler> = {};
		connect = connect;
		disconnect = disconnect;
		constructor() {
			state.lastRoom = this;
		}
		on(event: string, cb: Handler) {
			this.handlers[event] = cb;
			return this;
		}
		emit(event: string, ...args: unknown[]) {
			this.handlers[event]?.(...args);
		}
	}

	const RoomEvent = { TrackSubscribed: 'trackSubscribed', Disconnected: 'disconnected' };
	const Track = { Kind: { Video: 'video', Audio: 'audio' } };
	return { connect, disconnect, state, FakeRoom, RoomEvent, Track };
});

vi.mock('livekit-client', () => ({
	Room: h.FakeRoom,
	RoomEvent: h.RoomEvent,
	Track: h.Track
}));

import { playLiveKitStream } from './livekitStreamPlayer';

function makeVideo(): HTMLVideoElement {
	return document.createElement('video');
}

describe('playLiveKitStream', () => {
	beforeEach(() => {
		h.connect.mockClear().mockResolvedValue(undefined);
		h.disconnect.mockClear().mockResolvedValue(undefined);
		h.state.lastRoom = null;
	});

	it('connects with the given url + token and reports streaming', async () => {
		const onStatus = vi.fn();
		const handle = playLiveKitStream({
			serverUrl: 'ws://localhost:20140',
			token: 'jwt-abc',
			room: 'lk_exec1__cam',
			video: makeVideo(),
			onStatus
		});
		await vi.waitFor(() => expect(h.connect).toHaveBeenCalled());
		expect(h.connect).toHaveBeenCalledWith('ws://localhost:20140', 'jwt-abc');
		await vi.waitFor(() => expect(onStatus).toHaveBeenCalledWith('streaming'));
		handle.stop();
	});

	it('attaches a subscribed video track to the bound element', async () => {
		const video = makeVideo();
		playLiveKitStream({
			serverUrl: 'ws://localhost:20140',
			token: 't',
			room: 'r',
			video,
			onStatus: vi.fn()
		});
		await vi.waitFor(() => expect(h.state.lastRoom).not.toBeNull());

		const attach = vi.fn();
		// Video track → should attach.
		h.state.lastRoom!.emit('trackSubscribed', { kind: 'video', attach });
		expect(attach).toHaveBeenCalledWith(video);

		// Audio track → should NOT attach to the video element.
		const audioAttach = vi.fn();
		h.state.lastRoom!.emit('trackSubscribed', { kind: 'audio', attach: audioAttach });
		expect(audioAttach).not.toHaveBeenCalled();
	});

	it('stop() disconnects the room and reports stopped', async () => {
		const onStatus = vi.fn();
		const handle = playLiveKitStream({
			serverUrl: 'ws://localhost:20140',
			token: 't',
			room: 'r',
			video: makeVideo(),
			onStatus
		});
		await vi.waitFor(() => expect(h.connect).toHaveBeenCalled());
		handle.stop();
		expect(h.disconnect).toHaveBeenCalled();
		expect(onStatus).toHaveBeenCalledWith('stopped');
		// Idempotent.
		h.disconnect.mockClear();
		handle.stop();
		expect(h.disconnect).not.toHaveBeenCalled();
	});

	it('reports an error when connect rejects', async () => {
		h.connect.mockRejectedValueOnce(new Error('boom'));
		const onStatus = vi.fn();
		playLiveKitStream({
			serverUrl: 'ws://localhost:20140',
			token: 't',
			room: 'r',
			video: makeVideo(),
			onStatus
		});
		await vi.waitFor(() => expect(onStatus).toHaveBeenCalledWith('error', 'boom'));
	});
});
