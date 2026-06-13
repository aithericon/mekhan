import { describe, it, expect, vi } from 'vitest';
import type { Page } from '$lib/api/client';
import {
	createFolderPage,
	renameFolderPage,
	deleteFolderPage,
	type PageListOps,
	type ListHandle
} from './folder-pages-logic';

// Minimal Page factory — only the fields the optimistic logic reads matter.
function mkPage(over: Partial<Page> & { id: string; title: string }): Page {
	return {
		workspace_id: 'ws',
		attached_kind: null,
		attached_id: null,
		folder_id: 'f1',
		created_by: 'u',
		updated_by: 'u',
		created_at: '2026-01-01T00:00:00Z',
		updated_at: '2026-01-01T00:00:00Z',
		...over
	} as Page;
}

// A plain get/set list handle standing in for createListState's `items`.
function mkList(initial: Page[] = []): ListHandle {
	let items = initial;
	return {
		get items() {
			return items;
		},
		set items(v: Page[]) {
			items = v;
		}
	};
}

const FOLDER = 'f1';

describe('createFolderPage', () => {
	it('appends the server-returned page on success', async () => {
		const created = mkPage({ id: 'p-new', title: 'Fresh' });
		const ops: PageListOps = {
			createPage: vi.fn(async () => created),
			updatePage: vi.fn(),
			deletePage: vi.fn()
		};
		const list = mkList([mkPage({ id: 'p1', title: 'A' })]);

		const out = await createFolderPage(ops, list, FOLDER, 'Fresh');

		expect(out).toBe(created);
		expect(ops.createPage).toHaveBeenCalledWith({ folder_id: FOLDER, title: 'Fresh' });
		expect(list.items.map((p) => p.id)).toEqual(['p1', 'p-new']);
	});

	it('defaults a blank title to "Untitled"', async () => {
		const ops: PageListOps = {
			createPage: vi.fn(async () => mkPage({ id: 'p2', title: 'Untitled' })),
			updatePage: vi.fn(),
			deletePage: vi.fn()
		};
		await createFolderPage(ops, mkList(), FOLDER, '   ');
		expect(ops.createPage).toHaveBeenCalledWith({ folder_id: FOLDER, title: 'Untitled' });
	});

	it('propagates failure and leaves the list untouched', async () => {
		const ops: PageListOps = {
			createPage: vi.fn(async () => {
				throw new Error('boom');
			}),
			updatePage: vi.fn(),
			deletePage: vi.fn()
		};
		const list = mkList([mkPage({ id: 'p1', title: 'A' })]);
		await expect(createFolderPage(ops, list, FOLDER, 'X')).rejects.toThrow('boom');
		expect(list.items.map((p) => p.id)).toEqual(['p1']);
	});
});

describe('renameFolderPage', () => {
	it('optimistically applies then commits the new title', async () => {
		const ops: PageListOps = {
			createPage: vi.fn(),
			updatePage: vi.fn(async () => mkPage({ id: 'p1', title: 'Renamed' })),
			deletePage: vi.fn()
		};
		const list = mkList([
			mkPage({ id: 'p1', title: 'Old' }),
			mkPage({ id: 'p2', title: 'Other' })
		]);

		const issued = await renameFolderPage(ops, list, 'p1', '  Renamed  ');

		expect(issued).toBe(true);
		expect(ops.updatePage).toHaveBeenCalledWith('p1', { title: 'Renamed' });
		expect(list.items.find((p) => p.id === 'p1')?.title).toBe('Renamed');
		expect(list.items.find((p) => p.id === 'p2')?.title).toBe('Other');
	});

	it('rolls back to the prior title on failure', async () => {
		let applied: string | undefined;
		const ops: PageListOps = {
			createPage: vi.fn(),
			updatePage: vi.fn(async () => {
				// capture the optimistic state at the moment of the request
				applied = list.items.find((p) => p.id === 'p1')?.title;
				throw new Error('nope');
			}),
			deletePage: vi.fn()
		};
		const list = mkList([mkPage({ id: 'p1', title: 'Old' })]);

		await expect(renameFolderPage(ops, list, 'p1', 'New')).rejects.toThrow('nope');
		expect(applied).toBe('New'); // it WAS optimistically applied mid-flight
		expect(list.items.find((p) => p.id === 'p1')?.title).toBe('Old'); // …then rolled back
	});

	it('no-ops (no request) on blank or unchanged title', async () => {
		const ops: PageListOps = {
			createPage: vi.fn(),
			updatePage: vi.fn(),
			deletePage: vi.fn()
		};
		const list = mkList([mkPage({ id: 'p1', title: 'Same' })]);

		expect(await renameFolderPage(ops, list, 'p1', '   ')).toBe(false);
		expect(await renameFolderPage(ops, list, 'p1', 'Same')).toBe(false);
		expect(await renameFolderPage(ops, list, 'missing', 'X')).toBe(false);
		expect(ops.updatePage).not.toHaveBeenCalled();
	});
});

describe('deleteFolderPage', () => {
	it('optimistically removes then confirms', async () => {
		const ops: PageListOps = {
			createPage: vi.fn(),
			updatePage: vi.fn(),
			deletePage: vi.fn(async () => {})
		};
		const list = mkList([
			mkPage({ id: 'p1', title: 'A' }),
			mkPage({ id: 'p2', title: 'B' }),
			mkPage({ id: 'p3', title: 'C' })
		]);

		await deleteFolderPage(ops, list, 'p2');

		expect(ops.deletePage).toHaveBeenCalledWith('p2');
		expect(list.items.map((p) => p.id)).toEqual(['p1', 'p3']);
	});

	it('rolls back to the original index on failure', async () => {
		let midFlight: string[] = [];
		const ops: PageListOps = {
			createPage: vi.fn(),
			updatePage: vi.fn(),
			deletePage: vi.fn(async () => {
				midFlight = list.items.map((p) => p.id); // row already gone here
				throw new Error('fail');
			})
		};
		const list = mkList([
			mkPage({ id: 'p1', title: 'A' }),
			mkPage({ id: 'p2', title: 'B' }),
			mkPage({ id: 'p3', title: 'C' })
		]);

		await expect(deleteFolderPage(ops, list, 'p2')).rejects.toThrow('fail');
		expect(midFlight).toEqual(['p1', 'p3']); // optimistically removed
		expect(list.items.map((p) => p.id)).toEqual(['p1', 'p2', 'p3']); // restored in place
	});

	it('no-ops on an unknown id', async () => {
		const ops: PageListOps = {
			createPage: vi.fn(),
			updatePage: vi.fn(),
			deletePage: vi.fn()
		};
		const list = mkList([mkPage({ id: 'p1', title: 'A' })]);
		await deleteFolderPage(ops, list, 'nope');
		expect(ops.deletePage).not.toHaveBeenCalled();
		expect(list.items.map((p) => p.id)).toEqual(['p1']);
	});
});
