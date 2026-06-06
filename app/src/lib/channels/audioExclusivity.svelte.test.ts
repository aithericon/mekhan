import { describe, it, expect, vi, beforeEach } from 'vitest';
import { claim, release, currentOwner, isOwner, _reset } from './audioExclusivity.svelte';

beforeEach(() => _reset());

describe('audioExclusivity — single-owner audio store', () => {
	it('starts with no owner', () => {
		expect(currentOwner()).toBeNull();
		expect(isOwner('a')).toBe(false);
	});

	it('records the claimant as owner', () => {
		claim('a', () => {});
		expect(currentOwner()).toBe('a');
		expect(isOwner('a')).toBe(true);
		expect(isOwner('b')).toBe(false);
	});

	it('steals sound: a new claim stops the prior owner', () => {
		const stopA = vi.fn();
		const stopB = vi.fn();
		claim('a', stopA);
		claim('b', stopB);
		expect(stopA).toHaveBeenCalledTimes(1);
		expect(stopB).not.toHaveBeenCalled();
		expect(currentOwner()).toBe('b');
	});

	it('re-claiming by the same owner does NOT call its own stop', () => {
		const stopA1 = vi.fn();
		const stopA2 = vi.fn();
		claim('a', stopA1);
		claim('a', stopA2);
		expect(stopA1).not.toHaveBeenCalled();
		expect(stopA2).not.toHaveBeenCalled();
		expect(currentOwner()).toBe('a');
	});

	it('release by the owner clears ownership without firing its stop', () => {
		const stopA = vi.fn();
		claim('a', stopA);
		release('a');
		expect(currentOwner()).toBeNull();
		expect(stopA).not.toHaveBeenCalled();
	});

	it('release by a non-owner is a no-op', () => {
		claim('a', () => {});
		release('b');
		expect(currentOwner()).toBe('a');
	});

	it('a throwing prior-owner stop does not break the new claim', () => {
		claim('a', () => {
			throw new Error('torn down');
		});
		expect(() => claim('b', () => {})).not.toThrow();
		expect(currentOwner()).toBe('b');
	});

	it('a reentrant release() from inside the prior stop is harmless', () => {
		// Owner a's stop releases itself — must not clobber the incoming owner b.
		claim('a', () => release('a'));
		claim('b', () => {});
		expect(currentOwner()).toBe('b');
	});
});
