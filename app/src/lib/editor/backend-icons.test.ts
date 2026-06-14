import { describe, expect, it } from 'vitest';
import Cpu from '@lucide/svelte/icons/cpu';
import Globe from '@lucide/svelte/icons/globe';
import Mail from '@lucide/svelte/icons/mail';

import Python from './brand-icons/Python.svelte';
import Docker from './brand-icons/Docker.svelte';
import Postgresql from './brand-icons/Postgresql.svelte';

import { iconByName } from './backend-icons';

describe('iconByName', () => {
	it('maps brand-slug names to vendored brand components', () => {
		// These pairings mirror the brand `icon:` slugs in
		// shared/backends/src/registry.rs — if a slug changes there, this catches
		// the silent fall-through to Cpu.
		expect(iconByName('python')).toBe(Python);
		expect(iconByName('docker')).toBe(Docker);
		expect(iconByName('postgresql')).toBe(Postgresql);
	});

	it('maps generic names to Lucide glyphs for brand-less backends', () => {
		expect(iconByName('globe')).toBe(Globe); // http
		expect(iconByName('mail')).toBe(Mail); // smtp
	});

	it('gives different executors different glyphs (not all Cpu)', () => {
		const py = iconByName('python');
		const http = iconByName('globe');
		const docker = iconByName('docker');
		expect(new Set([py, http, docker]).size).toBe(3);
	});

	it('falls back to Cpu for empty / unknown names', () => {
		expect(iconByName(undefined)).toBe(Cpu);
		expect(iconByName(null)).toBe(Cpu);
		expect(iconByName('')).toBe(Cpu);
		expect(iconByName('no-such-icon')).toBe(Cpu);
	});
});
