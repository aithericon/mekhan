import { describe, expect, it } from 'vitest';
import {
	asItemsArray,
	getAtPath,
	interpolateRowPlaceholders,
	parseRepeaterRef
} from './task-form-values.svelte';

describe('parseRepeaterRef', () => {
	it('parses a bare iteration ref', () => {
		expect(parseRepeaterRef('extract.tasks[*]')).toEqual({
			head: 'extract',
			pre: ['tasks'],
			post: []
		});
	});

	it('parses a ref with a post-iteration field', () => {
		expect(parseRepeaterRef('extract.tasks[*].title')).toEqual({
			head: 'extract',
			pre: ['tasks'],
			post: ['title']
		});
	});

	it('parses a multi-segment pre path', () => {
		expect(parseRepeaterRef('extract.outer.inner[*].title')).toEqual({
			head: 'extract',
			pre: ['outer', 'inner'],
			post: ['title']
		});
	});

	it('rejects refs missing [*]', () => {
		expect(parseRepeaterRef('extract.tasks')).toBeNull();
	});

	it('rejects nested [*]', () => {
		expect(parseRepeaterRef('extract.tasks[*].sub[*].x')).toBeNull();
	});

	it('rejects empty input', () => {
		expect(parseRepeaterRef('')).toBeNull();
		expect(parseRepeaterRef('   ')).toBeNull();
	});

	it('rejects missing head', () => {
		expect(parseRepeaterRef('.tasks[*]')).toBeNull();
	});

	it('tolerates surrounding whitespace', () => {
		expect(parseRepeaterRef('  llm.items[*]  ')).toEqual({
			head: 'llm',
			pre: ['items'],
			post: []
		});
	});
});

describe('getAtPath', () => {
	it('returns nested values', () => {
		const data = { extract: { tasks: [{ title: 'a' }] } };
		expect(getAtPath(data, ['extract', 'tasks'])).toEqual([{ title: 'a' }]);
		expect(getAtPath(data, ['extract', 'tasks', '0', 'title'])).toBe('a');
	});

	it('returns undefined for missing keys', () => {
		expect(getAtPath({}, ['missing'])).toBeUndefined();
		expect(getAtPath({ a: 1 }, ['a', 'b'])).toBeUndefined();
	});

	it('returns undefined for null / non-object hops', () => {
		expect(getAtPath(null, ['a'])).toBeUndefined();
		expect(getAtPath({ a: null }, ['a', 'b'])).toBeUndefined();
		expect(getAtPath({ a: 'string' }, ['a', 'b'])).toBeUndefined();
	});

	it('empty path returns the input', () => {
		const data = { a: 1 };
		expect(getAtPath(data, [])).toBe(data);
	});
});

describe('asItemsArray', () => {
	it('returns arrays unchanged', () => {
		expect(asItemsArray([1, 2, 3])).toEqual([1, 2, 3]);
	});

	it('coerces non-arrays to empty', () => {
		expect(asItemsArray(undefined)).toEqual([]);
		expect(asItemsArray(null)).toEqual([]);
		expect(asItemsArray({ a: 1 })).toEqual([]);
		expect(asItemsArray('hello')).toEqual([]);
	});
});

describe('interpolateRowPlaceholders', () => {
	const parsed = { head: 'extract', pre: ['tasks'] };
	const item = { title: 'Buy widgets', amount: 42, meta: { vendor: 'Acme' } };

	it('substitutes a matching scalar leaf', () => {
		expect(
			interpolateRowPlaceholders('Review: {{ extract.tasks[*].title }}', parsed, item)
		).toBe('Review: Buy widgets');
	});

	it('substitutes a matching nested leaf', () => {
		expect(
			interpolateRowPlaceholders('Vendor {{ extract.tasks[*].meta.vendor }}', parsed, item)
		).toBe('Vendor Acme');
	});

	it('stringifies numeric leaves', () => {
		expect(
			interpolateRowPlaceholders('${{ extract.tasks[*].amount }}', parsed, item)
		).toBe('$42');
	});

	it('passes non-matching placeholders through unchanged', () => {
		expect(
			interpolateRowPlaceholders('Hello {{ start.user }}', parsed, item)
		).toBe('Hello {{ start.user }}');
	});

	it('passes placeholders without [*] through unchanged', () => {
		expect(
			interpolateRowPlaceholders('Static {{ extract.foo }}', parsed, item)
		).toBe('Static {{ extract.foo }}');
	});

	it('emits empty string for missing leaves', () => {
		expect(
			interpolateRowPlaceholders('Missing: {{ extract.tasks[*].nope }}', parsed, item)
		).toBe('Missing: ');
	});

	it('returns the source unchanged when no placeholders', () => {
		expect(interpolateRowPlaceholders('just text', parsed, item)).toBe('just text');
	});
});
