import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { UserProfileDto } from '$lib/api/client';

// Mocked batch resolver — the cache must funnel everything through this, and
// we assert on its call count to prove coalescing/dedup.
const resolveProfiles = vi.fn<(ids: string[]) => Promise<UserProfileDto[]>>();
vi.mock('$lib/api/client', () => ({ resolveProfiles: (ids: string[]) => resolveProfiles(ids) }));
vi.mock('$lib/auth/store.svelte', () => ({ auth: { session: null } }));

import { profiles } from './profiles.svelte';

const flush = () => new Promise<void>((r) => queueMicrotask(() => queueMicrotask(r)));

// Distinct ids per test so the singleton cache can't bleed between cases.
let n = 0;
const uid = () => `00000000-0000-0000-0000-${String(++n).padStart(12, '0')}`;

beforeEach(() => {
	resolveProfiles.mockReset();
});

describe('ProfileCache', () => {
	it('coalesces many ensure() calls in a tick into ONE batch request', async () => {
		const a = uid(), b = uid(), c = uid();
		resolveProfiles.mockResolvedValueOnce([
			{ user_id: a, display_name: 'A' },
			{ user_id: b, display_name: 'B' },
			{ user_id: c, display_name: 'C' }
		]);

		profiles.ensure([a]);
		profiles.ensure([b, c]);
		await flush();

		expect(resolveProfiles).toHaveBeenCalledTimes(1);
		expect(resolveProfiles).toHaveBeenCalledWith([a, b, c]);
		expect(profiles.get(a)?.display_name).toBe('A');
		expect(profiles.get(c)?.display_name).toBe('C');
	});

	it('does not re-request ids already cached or in flight', async () => {
		const a = uid();
		resolveProfiles.mockResolvedValueOnce([{ user_id: a, display_name: 'A' }]);
		profiles.ensure([a]);
		await flush();
		expect(resolveProfiles).toHaveBeenCalledTimes(1);

		// Second ensure for the same (now-cached) id must not fire again.
		profiles.ensure([a]);
		await flush();
		expect(resolveProfiles).toHaveBeenCalledTimes(1);
	});

	it('negative-caches unknown ids (resolved as null, never re-requested)', async () => {
		const ghost = uid();
		resolveProfiles.mockResolvedValueOnce([]); // server omits unknown ids
		profiles.ensure([ghost]);
		await flush();

		expect(profiles.get(ghost)).toBeNull(); // resolved-but-missing
		profiles.ensure([ghost]);
		await flush();
		expect(resolveProfiles).toHaveBeenCalledTimes(1); // not re-requested
	});

	it('seed() makes a denormalized profile resolvable without a request', async () => {
		const a = uid();
		profiles.seed({ user_id: a, display_name: 'Seeded', email: 's@x' });
		expect(profiles.get(a)?.display_name).toBe('Seeded');
		profiles.ensure([a]);
		await flush();
		expect(resolveProfiles).not.toHaveBeenCalled();
	});
});
