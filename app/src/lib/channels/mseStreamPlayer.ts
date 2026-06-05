/**
 * Play a LIVE fragmented-media byte stream (audio or video) through Media Source
 * Extensions, appending each segment the moment it arrives so playback starts
 * while the producer is still emitting.
 *
 * This is the MSE render adapter the {@link planLiveRender} registry dispatches
 * to for non-PCM containers. The datastream tap
 * (`GET .../channels/{c}/data?follow=1`) serves the channel's bytes as an
 * HTTP-chunked stream; for a fragmented MP4 / WebM the producer wrote (init
 * segment followed by media segments), MSE can append them progressively into a
 * `SourceBuffer` and the attached `<audio>`/`<video>` element decodes them as
 * they land — the container-codec analog of the raw-PCM Web Audio path.
 *
 * Mirrors {@link import('$lib/audio/livePcmPlayer').playLivePcm}'s shape (a
 * `stop()`-able handle, `onStatus`/`onProgress` callbacks) so the panel drives
 * both live renderers uniformly.
 *
 * NOTE: `appendBuffer` is asynchronous — a SourceBuffer can hold exactly one
 * in-flight append, signalled by its `updateend` event. We therefore await each
 * append before reading the next chunk, which doubles as natural back-pressure
 * (a fast producer can't outrun the decoder's buffer).
 */

export type LiveMediaStatus = 'streaming' | 'ended' | 'stopped' | 'error';

export interface LiveMediaHandle {
	/** Stop playback, cancel the network read, detach the MediaSource. */
	stop(): void;
}

export interface MseStreamOptions {
	/** The tap response's `ReadableStream` body (`?follow=1`). */
	stream: ReadableStream<Uint8Array>;
	/** Full MIME incl. `codecs=` — passed to `addSourceBuffer` verbatim. */
	mimeType: string;
	/** The `<audio>`/`<video>` element the MediaSource is attached to. */
	media: HTMLMediaElement;
	onStatus?: (status: LiveMediaStatus, error?: string) => void;
	/** Reports `(bufferedSeconds, bytesReceived)` as segments land. */
	onProgress?: (bufferedSeconds: number, bytesReceived: number) => void;
}

/** Last buffered timestamp, or 0 — guarded (TimeRanges throws on empty access). */
function bufferedEnd(sb: SourceBuffer): number {
	try {
		const ranges = sb.buffered;
		return ranges.length ? ranges.end(ranges.length - 1) : 0;
	} catch {
		return 0;
	}
}

/** Append one segment and resolve when the SourceBuffer finishes the update. */
function appendSegment(sb: SourceBuffer, chunk: Uint8Array): Promise<void> {
	return new Promise((resolve, reject) => {
		const onUpdateEnd = () => {
			cleanup();
			resolve();
		};
		const onError = () => {
			cleanup();
			reject(new Error('SourceBuffer append error'));
		};
		const cleanup = () => {
			sb.removeEventListener('updateend', onUpdateEnd);
			sb.removeEventListener('error', onError);
		};
		sb.addEventListener('updateend', onUpdateEnd);
		sb.addEventListener('error', onError);
		try {
			// `appendBuffer` types its arg as `ArrayBuffer`-backed (the lib excludes
			// SharedArrayBuffer); a tap chunk is always ArrayBuffer-backed, so this
			// cast is sound.
			sb.appendBuffer(chunk as unknown as BufferSource);
		} catch (e) {
			cleanup();
			reject(e instanceof Error ? e : new Error(String(e)));
		}
	});
}

/**
 * Start live MSE playback into `media`. Returns a handle whose `stop()` tears
 * everything down. Call from a user gesture (button click) so the browser lets
 * the media element start playing.
 */
export function playMseStream(opts: MseStreamOptions): LiveMediaHandle {
	const { stream, mimeType, media, onStatus, onProgress } = opts;
	const mediaSource = new MediaSource();
	const objectUrl = URL.createObjectURL(mediaSource);
	media.src = objectUrl;

	let stopped = false;
	let bytesReceived = 0;
	const reader = stream.getReader();

	const onSourceOpen = async () => {
		mediaSource.removeEventListener('sourceopen', onSourceOpen);
		// The object URL has served its purpose (the MediaSource is attached).
		URL.revokeObjectURL(objectUrl);

		let sourceBuffer: SourceBuffer;
		try {
			sourceBuffer = mediaSource.addSourceBuffer(mimeType);
		} catch (e) {
			if (!stopped) onStatus?.('error', e instanceof Error ? e.message : String(e));
			return;
		}

		onStatus?.('streaming');
		// Autoplay may still be blocked; the click that invoked us is the gesture.
		void media.play?.()?.catch?.(() => {});

		try {
			for (;;) {
				const { done, value } = await reader.read();
				if (stopped) break;
				if (done) break;
				if (!value || value.length === 0) continue;
				bytesReceived += value.length;
				await appendSegment(sourceBuffer, value);
				onProgress?.(bufferedEnd(sourceBuffer), bytesReceived);
			}
			if (!stopped && mediaSource.readyState === 'open') {
				try {
					mediaSource.endOfStream();
				} catch {
					/* already closed */
				}
				onStatus?.('ended');
			}
		} catch (e) {
			if (!stopped) onStatus?.('error', e instanceof Error ? e.message : String(e));
		}
	};

	mediaSource.addEventListener('sourceopen', onSourceOpen);

	return {
		stop() {
			if (stopped) return;
			stopped = true;
			void reader.cancel().catch(() => {});
			try {
				if (mediaSource.readyState === 'open') mediaSource.endOfStream();
			} catch {
				/* ignore */
			}
			try {
				media.pause?.();
			} catch {
				/* ignore */
			}
			media.removeAttribute('src');
			try {
				media.load?.();
			} catch {
				/* ignore */
			}
			onStatus?.('stopped');
		}
	};
}
