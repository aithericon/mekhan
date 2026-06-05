import { getContext, setContext } from 'svelte';
import type { Folder } from '$lib/api/client';

/**
 * Shared reactive handle for the folder detail layout and its tab subroutes
 * (templates / api / settings). The layout owns a single `$state` object and
 * provides it here; subpages read `folder`/`loading`/`error` reactively and
 * call `reload()` after mutations. Mirrors the instance-context pattern.
 */
export interface FolderContext {
	folderId: string;
	/** Owning workspace of the loaded folder (source of truth for bundle URLs). */
	workspaceId: string;
	folder: Folder | null;
	loading: boolean;
	error: string | null;
	reload: () => Promise<void>;
}

const KEY = Symbol('folder-context');

export function provideFolderContext(ctx: FolderContext): void {
	setContext(KEY, ctx);
}

export function getFolderContext(): FolderContext {
	return getContext(KEY);
}
