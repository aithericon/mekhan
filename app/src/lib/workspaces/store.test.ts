import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

// We import the store dynamically after stubbing the API client so the
// store's singleton instance reads our mock. The module is loaded once
// per test via `vi.resetModules()` for isolation.

vi.mock('$lib/api/client', () => ({
	listWorkspaces: vi.fn(),
	setActiveWorkspace: vi.fn()
}));

vi.mock('$lib/auth/store.svelte', () => ({
	auth: { session: null as { user: { workspaceId?: string } } | null }
}));

import * as apiMock from '$lib/api/client';
import * as authMock from '$lib/auth/store.svelte';

describe('WorkspaceStore', () => {
	beforeEach(async () => {
		vi.resetModules();
		(apiMock.listWorkspaces as unknown as ReturnType<typeof vi.fn>).mockReset();
		(apiMock.setActiveWorkspace as unknown as ReturnType<typeof vi.fn>).mockReset();
		(authMock as { auth: { session: unknown } }).auth.session = null;
	});

	afterEach(() => {
		vi.restoreAllMocks();
	});

	it('load() populates the workspace list once', async () => {
		(apiMock.listWorkspaces as ReturnType<typeof vi.fn>).mockResolvedValue([
			{
				id: 'ws-1',
				slug: 'a',
				display_name: 'A',
				is_system: false,
				created_at: '2026-01-01T00:00:00Z'
			}
		]);
		const { workspaces } = await import('./store.svelte');
		await workspaces.load();
		expect(workspaces.workspaces).toHaveLength(1);
		expect(workspaces.loaded).toBe(true);
		// Second call is a no-op (idempotent guard).
		await workspaces.load();
		expect(apiMock.listWorkspaces).toHaveBeenCalledTimes(1);
	});

	it('refresh() force-refetches even when loaded', async () => {
		const mock = apiMock.listWorkspaces as ReturnType<typeof vi.fn>;
		mock
			.mockResolvedValueOnce([
				{ id: 'a', slug: 'a', display_name: 'A', is_system: false, created_at: '' }
			])
			.mockResolvedValueOnce([
				{ id: 'a', slug: 'a', display_name: 'A', is_system: false, created_at: '' },
				{ id: 'b', slug: 'b', display_name: 'B', is_system: false, created_at: '' }
			]);
		const { workspaces } = await import('./store.svelte');
		await workspaces.load();
		expect(workspaces.workspaces).toHaveLength(1);
		await workspaces.refresh();
		expect(workspaces.workspaces).toHaveLength(2);
	});

	it('active derives from auth.session.user.workspaceId', async () => {
		(apiMock.listWorkspaces as ReturnType<typeof vi.fn>).mockResolvedValue([
			{ id: 'ws-1', slug: 'a', display_name: 'A', is_system: false, created_at: '' },
			{ id: 'ws-2', slug: 'b', display_name: 'B', is_system: false, created_at: '' }
		]);
		const { workspaces } = await import('./store.svelte');
		await workspaces.load();

		expect(workspaces.active).toBeNull();
		(authMock as { auth: { session: unknown } }).auth.session = {
			user: { workspaceId: 'ws-2' }
		};
		expect(workspaces.active?.slug).toBe('b');
	});

	it('switchTo POSTs to API + reloads window', async () => {
		(apiMock.listWorkspaces as ReturnType<typeof vi.fn>).mockResolvedValue([]);
		(apiMock.setActiveWorkspace as ReturnType<typeof vi.fn>).mockResolvedValue(undefined);
		const { workspaces } = await import('./store.svelte');

		const reloadSpy = vi.fn();
		Object.defineProperty(window, 'location', {
			writable: true,
			value: { reload: reloadSpy }
		});

		await workspaces.switchTo('ws-7');
		expect(apiMock.setActiveWorkspace).toHaveBeenCalledWith('ws-7');
		expect(reloadSpy).toHaveBeenCalled();
	});

	it('load() falls back to empty list on API failure', async () => {
		(apiMock.listWorkspaces as ReturnType<typeof vi.fn>).mockRejectedValue(
			new Error('network down')
		);
		const { workspaces } = await import('./store.svelte');
		await workspaces.load();
		expect(workspaces.workspaces).toEqual([]);
		expect(workspaces.loaded).toBe(false); // didn't successfully load
	});
});
