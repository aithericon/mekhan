import { describe, it, expect, beforeEach } from 'vitest';
import { EntriesQueryState } from './entries-query.svelte';

// jsdom provides window/history; the $app/environment stub reports
// browser=true, so apply() exercises the real ?q= sync path.

describe('EntriesQueryState', () => {
	beforeEach(() => {
		history.replaceState(null, '', '/data');
	});

	it('hydrates applied + draft from the initial query', () => {
		const s = new EntriesQueryState('format:csv');
		expect(s.applied).toBe('format:csv');
		expect(s.draft).toBe('format:csv');
		expect(s.page).toBe(0);
	});

	it('apply() sets both texts, resets paging, and syncs ?q=', () => {
		const s = new EntriesQueryState('');
		s.page = 3;
		s.apply('category:dataset');
		expect(s.applied).toBe('category:dataset');
		expect(s.draft).toBe('category:dataset');
		expect(s.page).toBe(0);
		expect(new URL(window.location.href).searchParams.get('q')).toBe('category:dataset');
	});

	it('apply("") removes ?q= from the URL', () => {
		const s = new EntriesQueryState('format:csv');
		s.apply('format:csv'); // write it
		s.apply('');
		expect(new URL(window.location.href).searchParams.get('q')).toBeNull();
	});

	it('addTerm() merges into the applied query and applies', () => {
		const s = new EntriesQueryState('format:csv');
		s.page = 2;
		s.addTerm('col:email');
		expect(s.applied).toBe('format:csv col:email');
		expect(s.draft).toBe(s.applied);
		expect(s.page).toBe(0);
	});

	it('insertDraft() touches only the draft — nothing executes', () => {
		const s = new EntriesQueryState('format:csv');
		s.insertDraft('meta.num_rows:');
		expect(s.draft).toBe('format:csv meta.num_rows:');
		expect(s.applied).toBe('format:csv');
		expect(new URL(window.location.href).searchParams.get('q')).toBeNull();
	});
});
