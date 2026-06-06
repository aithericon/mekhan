import { describe, it, expect } from 'vitest';
import { sanitizeChannelName, defaultElement, newChannel } from './channel-authoring';

describe('sanitizeChannelName', () => {
	it('lower-cases and collapses non-identifier runs to a single underscore', () => {
		expect(sanitizeChannelName('My Channel')).toBe('my_channel');
		expect(sanitizeChannelName('frame-stream!!')).toBe('frame_stream_');
		expect(sanitizeChannelName('A  B  C')).toBe('a_b_c');
	});

	it('strips leading underscores (reserved for input._* control leaves)', () => {
		expect(sanitizeChannelName('__media')).toBe('media');
		expect(sanitizeChannelName('  speech')).toBe('speech');
	});

	it('keeps already-valid identifiers intact', () => {
		expect(sanitizeChannelName('media')).toBe('media');
		expect(sanitizeChannelName('parts_2')).toBe('parts_2');
	});
});

describe('defaultElement — rebuilds the union per kind', () => {
	it('json → an empty-schema json element (no stray content_type)', () => {
		const el = defaultElement('json');
		expect(el).toEqual({ type: 'json', schema: {} });
		expect('content_type' in el).toBe(false);
	});

	it('binary → octet-stream content_type (no stray schema)', () => {
		const el = defaultElement('binary');
		expect(el).toEqual({ type: 'binary', content_type: 'application/octet-stream' });
		expect('schema' in el).toBe(false);
	});

	it('any → a bare passthrough', () => {
		const el = defaultElement('any');
		expect(el).toEqual({ type: 'any' });
		expect('schema' in el).toBe(false);
		expect('content_type' in el).toBe(false);
	});
});

describe('newChannel', () => {
	it('seeds a durable binary data-OUT channel with a blank name', () => {
		expect(newChannel()).toEqual({
			name: '',
			direction: 'out',
			plane: 'data',
			element: { type: 'binary', content_type: 'application/octet-stream' },
			transport: 'jetstream'
		});
	});
});
