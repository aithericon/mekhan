import { describe, it, expect } from 'vitest';
import { matchesMedia, pickRenderer } from './index';
import type { RenderContext } from './types';

const ctx: RenderContext = { position: 'output' };

describe('matchesMedia', () => {
	it('matches a media file ref ({url, content_type: audio/*})', () => {
		expect(matchesMedia({ url: 'https://x/a.wav', content_type: 'audio/wav' })).toBe(true);
		expect(matchesMedia({ url: 'https://x/v.mp4', content_type: 'video/mp4' })).toBe(true);
		expect(matchesMedia({ url: 'https://x/i.png', content_type: 'image/png' })).toBe(true);
	});

	it('matches a data: media URL', () => {
		expect(matchesMedia('data:audio/wav;base64,UklGRiQAAAB')).toBe(true);
		expect(matchesMedia('data:video/mp4;base64,AAAA')).toBe(true);
		expect(matchesMedia('data:image/png;base64,iVBOR')).toBe(true);
	});

	it('rejects a non-media file ref', () => {
		expect(matchesMedia({ url: 'https://x/doc.pdf', content_type: 'application/pdf' })).toBe(false);
		// No content_type at all → not media (still a plain file ref).
		expect(matchesMedia({ url: 'https://x/whatever' })).toBe(false);
	});

	it('rejects a plain transcript / non-media string', () => {
		expect(matchesMedia('the quick brown fox')).toBe(false);
		expect(matchesMedia('data:text/plain;base64,aGk=')).toBe(false);
		expect(matchesMedia('data:application/json,{}')).toBe(false);
	});

	it('rejects non-object / non-string values', () => {
		expect(matchesMedia(null)).toBe(false);
		expect(matchesMedia(42)).toBe(false);
		expect(matchesMedia(['audio/wav'])).toBe(false);
	});
});

describe('pickRenderer dispatch ordering', () => {
	it('routes a media file ref to MediaPlayer (out-ranking file-ref)', () => {
		const picked = pickRenderer({ url: 'https://x/a.wav', content_type: 'audio/wav' }, ctx);
		expect(picked.name).toBe('media-player');
	});

	it('routes a data:audio URL to MediaPlayer (out-ranking primitive)', () => {
		const picked = pickRenderer('data:audio/wav;base64,UklGRiQAAAB', ctx);
		expect(picked.name).toBe('media-player');
	});

	it('routes a plain (non-media) file ref to FileReference', () => {
		const picked = pickRenderer({ url: 'https://x/doc.pdf', content_type: 'application/pdf' }, ctx);
		expect(picked.name).toBe('file-ref');
	});

	it('does NOT route a plain transcript string to MediaPlayer', () => {
		const picked = pickRenderer('the quick brown fox jumped', ctx);
		expect(picked.name).not.toBe('media-player');
	});
});
