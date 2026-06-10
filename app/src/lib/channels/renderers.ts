/**
 * Channel render-adapter registry — the PRESENTATION-side analog of the
 * data-plane transport dispatch (docs/25 §6).
 *
 * On the wire, the producer's `open` descriptor carries a `transport` tag and
 * both executors dispatch their byte adapter off it (JetStream / nats-latest /
 * s3). Here the SAME idea runs one layer up, in the browser: a data channel's
 * element **content_type** is the descriptor, and we dispatch the matching
 * *render* adapter off it — raw PCM plays through Web Audio, fragmented
 * audio/video plays live through Media Source Extensions, and an
 * already-complete blob plays through a native `<audio>`/`<video>`/`<img>`
 * element. One classifier, three renderers, selected by data — not an
 * ever-growing `if` ladder in the panel.
 *
 * This module is the single, pure, unit-tested place that classification lives,
 * so the panel never re-derives "is this playable?" ad hoc. Adding a new render
 * path (e.g. a future HLS adapter) is a new arm here, not a new branch there.
 */

import { isRawPcm } from '$lib/audio/pcmWav';

/** A live (follow-the-stream) renderer choice for a data channel. */
export type LiveRenderKind =
	/** Headerless little-endian PCM → schedule on a Web Audio timeline. */
	| 'pcm'
	/** Fragmented audio/video the browser's MSE can append progressively. */
	| 'mse'
	/** A stream of self-contained JPEG frames → swap each into an `<img>`. */
	| 'mjpeg'
	/** A robot joint-angle (joint-state) stream → drive a 3D URDF twin. */
	| 'urdf'
	/** A planning-scene stream → drive a 3D twin of the arm + collision objects. */
	| 'scene'
	/** A `text/*` byte stream → decode UTF-8 and append into a live console tail. */
	| 'text';

export interface LiveRenderPlan {
	kind: LiveRenderKind;
	/** Which media element the bytes drive (MSE needs one; PCM uses AudioContext;
	 *  MJPEG swaps frames into an `<img>`; URDF drives a `'3d'` scene; text
	 *  appends into a scrolling `<pre>`). */
	mediaKind: 'audio' | 'video' | 'image' | '3d' | 'text';
	/** The content_type to hand the renderer (the full MIME incl. any `codecs=`
	 *  param — MSE's `addSourceBuffer`/`isTypeSupported` require codecs). */
	mime: string;
}

/** A whole-file (already-complete) renderer choice: a native media element. */
export interface FileRenderPlan {
	mediaKind: 'audio' | 'video' | 'image';
}

/** The base (pre-`;param`) MIME, lower-cased and trimmed. */
function baseType(contentType: string): string {
	return contentType.split(';', 1)[0].trim().toLowerCase();
}

/**
 * The default MSE capability probe — `MediaSource.isTypeSupported`, or a
 * never-supported stub when MSE is absent (SSR, jsdom, or a browser without it).
 * Injectable in {@link planLiveRender} so the classifier is unit-testable
 * without a real `MediaSource`.
 */
export function defaultMseSupported(mime: string): boolean {
	if (typeof MediaSource === 'undefined' || typeof MediaSource.isTypeSupported !== 'function') {
		return false;
	}
	return MediaSource.isTypeSupported(mime);
}

/**
 * Classify how a data channel's `content_type` should be played LIVE (streamed
 * as it lands via the `?follow=1` tap), or `null` when it has no live renderer.
 *
 * Dispatch order:
 *  1. raw PCM (`audio/L16` / `audio/pcm`) → Web Audio (`kind: 'pcm'`). `<audio>`
 *     can't progressively decode headerless PCM, so we schedule samples
 *     ourselves.
 *  2. an `audio/*` or `video/*` MIME the browser's MSE supports (it must carry a
 *     `codecs=` param the UA can decode — e.g. `audio/webm;codecs="opus"`,
 *     `video/mp4;codecs="avc1.42E01E,mp4a.40.2"`) → MSE (`kind: 'mse'`).
 *  3. otherwise → `null` (no live path; a whole-file preview may still apply —
 *     see {@link planFileRender}).
 *
 * `mseSupported` is injected for testability; production passes
 * {@link defaultMseSupported}.
 */
export function planLiveRender(
	contentType: string | null | undefined,
	mseSupported: (mime: string) => boolean = defaultMseSupported
): LiveRenderPlan | null {
	if (!contentType) return null;
	// PCM first — it overlaps `audio/*` but has its own (non-MSE) renderer.
	if (isRawPcm(contentType)) {
		return { kind: 'pcm', mediaKind: 'audio', mime: contentType };
	}
	const base = baseType(contentType);
	const isAudio = base.startsWith('audio/');
	const isVideo = base.startsWith('video/');
	if ((isAudio || isVideo) && mseSupported(contentType)) {
		return { kind: 'mse', mediaKind: isVideo ? 'video' : 'audio', mime: contentType };
	}
	// Motion-JPEG: a data channel of self-contained JPEG frames (e.g. a live
	// camera / annotated-detection feed). No MSE probe — every UA decodes JPEG in
	// an `<img>`; the player re-frames the byte stream on JPEG EOI markers. Only
	// `image/jpeg` (the EOI split is JPEG-specific); other `image/*` fall through.
	if (base === 'image/jpeg') {
		return { kind: 'mjpeg', mediaKind: 'image', mime: contentType };
	}
	// URDF twin: a data channel of robot joint-angle (joint-state) records (one
	// NDJSON object per tick). No MSE probe — it isn't a media container; the
	// player feeds each joint-state frame into a 3D URDF model rendered live.
	// Only this exact base type (the NDJSON joint-state shape); others fall through.
	if (base === 'application/vnd.aithericon.joint-state+x-ndjson') {
		return { kind: 'urdf', mediaKind: '3d', mime: contentType };
	}
	// Planning-scene twin: a data channel of full planning-scene snapshots (one
	// NDJSON object per tick) carrying the arm pose + world collision objects +
	// any attached (grasped) object. The sibling of the joint-state 'urdf' arm —
	// the player feeds each snapshot into a 3D twin of the arm AND its scene.
	if (base === 'application/vnd.aithericon.planning-scene+x-ndjson') {
		return { kind: 'scene', mediaKind: '3d', mime: contentType };
	}
	// Live text tail: any `text/*` data channel (text/plain echo feeds, CSV/log
	// streams, LLM token streams) decodes as UTF-8 and appends into a scrolling
	// console. No probe needed — every UA can render text. The whole `text/`
	// family takes this arm; structured `application/*` types (json, ndjson)
	// deliberately do NOT — they carry framing a raw tail would mangle.
	if (base.startsWith('text/')) {
		return { kind: 'text', mediaKind: 'text', mime: contentType };
	}
	return null;
}

/**
 * Classify how a data channel's `content_type` should be played as a
 * whole FILE (fetched complete, then handed to a native element), or `null`
 * when it isn't a media type a native element renders.
 *
 * This is the refactor of the panel's old ad-hoc `isPlayable` regex: any
 * `audio/*` / `video/*` / `image/*` content_type is previewable (raw PCM
 * included — the caller WAV-wraps it before handing it to `<audio>`).
 */
export function planFileRender(contentType: string | null | undefined): FileRenderPlan | null {
	if (!contentType) return null;
	const base = baseType(contentType);
	if (base.startsWith('audio/')) return { mediaKind: 'audio' };
	if (base.startsWith('video/')) return { mediaKind: 'video' };
	if (base.startsWith('image/')) return { mediaKind: 'image' };
	return null;
}
