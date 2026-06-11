import { describe, it, expect } from 'vitest';
import { initialsFor, colorFor } from './Avatar.svelte';

describe('initialsFor', () => {
	it('takes first + last initial of a multi-word name', () => {
		expect(initialsFor('Dev User')).toBe('DU');
		expect(initialsFor('Ada Lovelace King')).toBe('AK');
	});

	it('takes two letters of a single-word name', () => {
		expect(initialsFor('alice')).toBe('AL');
		expect(initialsFor('X')).toBe('X');
	});

	it('falls back to the email local-part when no name', () => {
		expect(initialsFor(null, 'bob@corp.com')).toBe('BO');
		expect(initialsFor(undefined, 'q@x')).toBe('Q');
	});

	it('falls back to the UUID head when no name or email', () => {
		expect(initialsFor(null, null, '3bb26085-29f3-5fbf')).toBe('3B');
	});

	it('returns ? when nothing is known', () => {
		expect(initialsFor()).toBe('?');
	});
});

describe('colorFor', () => {
	it('is deterministic per seed', () => {
		expect(colorFor('user-1')).toBe(colorFor('user-1'));
	});

	it('returns a palette class for a seed and a neutral class for none', () => {
		expect(colorFor('user-1')).toMatch(/^bg-/);
		expect(colorFor(null)).toBe('bg-muted-foreground');
	});
});
