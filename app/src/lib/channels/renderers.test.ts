import { describe, it, expect } from 'vitest';
import { planLiveRender, planFileRender, type LiveRenderPlan } from './renderers';

// A capability probe stub: only the MIMEs in `supported` are MSE-playable.
const supports = (supported: string[]) => (mime: string) => supported.includes(mime);
const supportsNone = () => false;
const supportsAll = () => true;

describe('planLiveRender — presentation-side content_type dispatch', () => {
	it('routes raw PCM to the Web Audio (pcm) renderer, never MSE', () => {
		// Even if MSE "supported everything", PCM takes its own arm (headerless
		// PCM is not an MSE container).
		const plan = planLiveRender('audio/L16;rate=16000', supportsAll);
		expect(plan).toEqual<LiveRenderPlan>({
			kind: 'pcm',
			mediaKind: 'audio',
			mime: 'audio/L16;rate=16000'
		});
	});

	it('treats audio/pcm as PCM too', () => {
		expect(planLiveRender('audio/pcm', supportsNone)?.kind).toBe('pcm');
	});

	it('routes an MSE-supported audio container to the mse renderer', () => {
		const mime = 'audio/webm;codecs="opus"';
		expect(planLiveRender(mime, supports([mime]))).toEqual<LiveRenderPlan>({
			kind: 'mse',
			mediaKind: 'audio',
			mime
		});
	});

	it('routes an MSE-supported video container to the mse renderer with mediaKind video', () => {
		const mime = 'video/mp4;codecs="avc1.42E01E,mp4a.40.2"';
		expect(planLiveRender(mime, supports([mime]))).toEqual<LiveRenderPlan>({
			kind: 'mse',
			mediaKind: 'video',
			mime
		});
	});

	it('returns null for an audio/video MIME the browser MSE does NOT support', () => {
		// e.g. a codecs combo this UA can't decode → no live MSE path.
		expect(planLiveRender('video/mp4;codecs="hev1.1.6.L93.B0"', supportsNone)).toBeNull();
	});

	it('routes an image/jpeg data channel to the mjpeg renderer (no MSE probe needed)', () => {
		// Even with MSE unsupported, a JPEG frame stream is renderable in an <img>.
		expect(planLiveRender('image/jpeg', supportsNone)).toEqual<LiveRenderPlan>({
			kind: 'mjpeg',
			mediaKind: 'image',
			mime: 'image/jpeg'
		});
	});

	it('ignores params/casing when classifying image/jpeg as mjpeg', () => {
		expect(planLiveRender('IMAGE/JPEG; foo=bar', supportsNone)?.kind).toBe('mjpeg');
	});

	it('does NOT treat other image/* as mjpeg (the EOI split is JPEG-specific)', () => {
		// image/png has different framing; no live image-sequence path for it.
		expect(planLiveRender('image/png', supportsAll)).toBeNull();
	});

	it('returns null for a non-media content_type', () => {
		expect(planLiveRender('application/octet-stream', supportsAll)).toBeNull();
		expect(planLiveRender('application/json', supportsAll)).toBeNull();
	});

	it('returns null for empty / missing content_type', () => {
		expect(planLiveRender(null)).toBeNull();
		expect(planLiveRender(undefined)).toBeNull();
		expect(planLiveRender('')).toBeNull();
	});

	it('does not offer MSE when the probe rejects, even for a plausible container', () => {
		// The classifier never assumes support — it asks the probe.
		expect(planLiveRender('audio/mp4;codecs="mp4a.40.2"', supportsNone)).toBeNull();
	});
});

describe('planFileRender — whole-file native-element dispatch', () => {
	it('classifies audio / video / image base types', () => {
		expect(planFileRender('audio/wav')).toEqual({ mediaKind: 'audio' });
		expect(planFileRender('audio/mpeg')).toEqual({ mediaKind: 'audio' });
		expect(planFileRender('video/mp4')).toEqual({ mediaKind: 'video' });
		expect(planFileRender('image/png')).toEqual({ mediaKind: 'image' });
	});

	it('treats raw PCM as a previewable audio file (caller WAV-wraps it)', () => {
		expect(planFileRender('audio/L16;rate=16000')).toEqual({ mediaKind: 'audio' });
	});

	it('ignores content_type parameters and casing', () => {
		expect(planFileRender('AUDIO/WAV; rate=44100')).toEqual({ mediaKind: 'audio' });
	});

	it('returns null for non-media types and empties', () => {
		expect(planFileRender('application/octet-stream')).toBeNull();
		expect(planFileRender('text/plain')).toBeNull();
		expect(planFileRender(null)).toBeNull();
		expect(planFileRender('')).toBeNull();
	});
});
