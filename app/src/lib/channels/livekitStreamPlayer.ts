/**
 * Play a LIVE WebRTC video track from a LiveKit room — the `livekit` transport's
 * render adapter, the real-time analog of the MSE / MJPEG live players.
 *
 * Unlike the byte-stream taps (PCM / MSE / MJPEG), which pull HTTP-chunked bytes
 * from mekhan's datastream endpoint and re-frame them client-side, the LiveKit
 * path subscribes to a media track published by the executor straight into a
 * LiveKit SFU room. mekhan only mints the (subscribe-only) join token + room
 * name; the actual video never transits mekhan. We connect to the room, and the
 * moment a Video track is subscribed (auto-subscribe is on by default) we
 * `attach` it to the bound `<video>` element so libwebrtc decodes it directly.
 *
 * Mirrors the other live players' shape — a `stop()`-able {@link LiveMediaHandle}
 * plus an `onStatus` callback over the shared {@link LiveMediaStatus} union — so
 * the panel drives every renderer uniformly.
 */

import { Room, RoomEvent, Track, type RemoteTrack } from 'livekit-client';
import type { LiveMediaHandle, LiveMediaStatus } from './mseStreamPlayer';

export interface LiveKitStreamOptions {
	/** LiveKit server WebSocket URL (e.g. `ws://localhost:20140`). */
	serverUrl: string;
	/** Subscribe-only room-join JWT minted by mekhan. */
	token: string;
	/** Room name (`lk_{execution_id}__{channel}`); informational — the token is
	 *  scoped to it server-side. Kept for symmetry / debugging. */
	room: string;
	/** The `<video>` element the subscribed video track is attached to. */
	video: HTMLVideoElement;
	onStatus?: (status: LiveMediaStatus, error?: string) => void;
}

/**
 * Connect to the LiveKit room and attach the first subscribed video track to the
 * bound `<video>` element. Returns a handle whose `stop()` disconnects the room
 * (which detaches the track).
 */
export function playLiveKitStream(opts: LiveKitStreamOptions): LiveMediaHandle {
	const { serverUrl, token, video, onStatus } = opts;
	let stopped = false;
	const room = new Room();

	room.on(RoomEvent.TrackSubscribed, (track: RemoteTrack) => {
		if (stopped) return;
		if (track.kind === Track.Kind.Video) {
			track.attach(video);
			// Some browsers (Firefox) don't honour the element's `autoplay` for a
			// freshly-attached MediaStream; nudge it. The element is `muted`, so
			// the play() is not autoplay-blocked.
			void video.play?.()?.catch?.(() => {});
		}
	});

	room.on(RoomEvent.Disconnected, () => {
		if (!stopped) onStatus?.('ended');
	});

	(async () => {
		try {
			await room.connect(serverUrl, token);
			if (stopped) {
				// Raced with stop() — tear the connection back down.
				await room.disconnect();
				return;
			}
			onStatus?.('streaming');
		} catch (e) {
			if (!stopped) onStatus?.('error', e instanceof Error ? e.message : String(e));
		}
	})();

	return {
		stop() {
			if (stopped) return;
			stopped = true;
			void room.disconnect();
			onStatus?.('stopped');
		}
	};
}
