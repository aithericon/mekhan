// Optimistic CRUD logic for the folder Pages panel, extracted from the Svelte
// component so it can be unit-tested without mounting (mirrors the inline
// optimistic pattern in routes/folders/+page.svelte — apply locally, roll back
// on failure). `list` is the mutable items array (the createListState `items`);
// these helpers replace it with a new array reference so Svelte's `$state`
// reactivity fires.

import type { Page } from '$lib/api/client';

export type PageListOps = {
	createPage: (body: { folder_id: string; title: string }) => Promise<Page>;
	updatePage: (id: string, body: { title?: string }) => Promise<Page>;
	deletePage: (id: string) => Promise<void>;
};

export type ListHandle = {
	get items(): Page[];
	set items(v: Page[]);
};

/**
 * Create a page in `folderId`. The server is the source of truth for the id /
 * timestamps, so we await it and append the returned row (no temp-id optimism
 * needed — creation is a single append with no prior state to roll back).
 * Returns the created page; throws on failure (caller surfaces the error).
 */
export async function createFolderPage(
	ops: PageListOps,
	list: ListHandle,
	folderId: string,
	title: string
): Promise<Page> {
	const created = await ops.createPage({ folder_id: folderId, title: title.trim() || 'Untitled' });
	list.items = [...list.items, created];
	return created;
}

/**
 * Inline rename: apply the new title locally, then PATCH. On failure restore
 * the prior title. No-op (and no request) when the title is blank/unchanged.
 * Returns `true` if a request was issued.
 */
export async function renameFolderPage(
	ops: PageListOps,
	list: ListHandle,
	id: string,
	nextRaw: string
): Promise<boolean> {
	const next = nextRaw.trim();
	const cur = list.items.find((p) => p.id === id);
	if (!cur || !next || next === cur.title) return false;
	const prev = cur.title;
	list.items = list.items.map((p) => (p.id === id ? { ...p, title: next } : p)); // optimistic
	try {
		await ops.updatePage(id, { title: next });
		return true;
	} catch (e) {
		list.items = list.items.map((p) => (p.id === id ? { ...p, title: prev } : p)); // rollback
		throw e;
	}
}

/**
 * Optimistically drop the row, then DELETE. On failure re-insert it at its
 * original index so ordering is preserved.
 */
export async function deleteFolderPage(
	ops: PageListOps,
	list: ListHandle,
	id: string
): Promise<void> {
	const idx = list.items.findIndex((p) => p.id === id);
	if (idx < 0) return;
	const removed = list.items[idx];
	list.items = list.items.filter((p) => p.id !== id); // optimistic
	try {
		await ops.deletePage(id);
	} catch (e) {
		const restored = [...list.items];
		restored.splice(idx, 0, removed); // rollback at original index
		list.items = restored;
		throw e;
	}
}
