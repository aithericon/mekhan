import { describe, it, expect } from 'vitest';
import { formatBytes, formatCount } from './format';

describe('formatBytes', () => {
	it('renders the null/undefined sentinel as an em-dash', () => {
		expect(formatBytes(null)).toBe('—');
		expect(formatBytes(undefined)).toBe('—');
	});

	it('renders zero and sub-KB values without decimals', () => {
		expect(formatBytes(0)).toBe('0 B');
		expect(formatBytes(512)).toBe('512 B');
	});

	it('scales through the binary units with one decimal', () => {
		expect(formatBytes(1536)).toBe('1.5 KB');
		expect(formatBytes(5 * 1024 ** 2)).toBe('5.0 MB');
		expect(formatBytes(2.5 * 1024 ** 3)).toBe('2.5 GB');
		expect(formatBytes(1024 ** 4)).toBe('1.0 TB');
	});

	it('clamps to the largest unit instead of indexing past it', () => {
		expect(formatBytes(1024 ** 6)).toBe('1024.0 PB');
	});
});

describe('formatCount', () => {
	it('renders the null/undefined sentinel as an em-dash', () => {
		expect(formatCount(null)).toBe('—');
		expect(formatCount(undefined)).toBe('—');
	});

	it('renders locale-grouped integers', () => {
		expect(formatCount(0)).toBe('0');
		expect(formatCount(1234567)).toBe((1234567).toLocaleString());
	});
});
