import { describe, it, expect } from 'vitest';
import { statusTone, shortId } from './model-pool';

describe('statusTone', () => {
	it('maps active/loaded to emerald', () => {
		expect(statusTone('active')).toContain('emerald');
		expect(statusTone('loaded')).toContain('emerald');
	});

	it('maps failed to red', () => {
		expect(statusTone('failed')).toContain('red');
	});

	it('maps stopped/unloaded to muted', () => {
		expect(statusTone('stopped')).toBe('text-muted-foreground');
		expect(statusTone('unloaded')).toBe('text-muted-foreground');
	});

	it('maps sleeping to indigo (before the amber fallback)', () => {
		expect(statusTone('sleeping')).toBe('text-indigo-500 dark:text-indigo-400');
	});

	it('falls back to amber for anything else', () => {
		expect(statusTone('loading')).toContain('amber');
		expect(statusTone('whatever')).toContain('amber');
	});
});

describe('shortId', () => {
	it('takes the first 8 chars', () => {
		expect(shortId('0123456789abcdef')).toBe('01234567');
	});
});
