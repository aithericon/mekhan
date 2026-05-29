/**
 * Workspace store — caches the caller's membership list and the active
 * workspace id. Loaded once at layout mount, refreshed on switch.
 *
 * Active id source-of-truth is `auth.session?.user.workspaceId` (which the
 * BFF resolves from cookie + membership). This store holds the *list* so
 * the picker UI doesn't refetch on every paint.
 */
import { auth } from '$lib/auth/store.svelte';
import {
	listWorkspaces,
	setActiveWorkspace as setActiveWorkspaceApi,
	type WorkspaceSummary
} from '$lib/api/client';

class WorkspaceStore {
	#workspaces = $state<WorkspaceSummary[]>([]);
	#loaded = $state(false);
	#loading = $state(false);

	get workspaces(): WorkspaceSummary[] {
		return this.#workspaces;
	}

	get loaded(): boolean {
		return this.#loaded;
	}

	get loading(): boolean {
		return this.#loading;
	}

	/** Currently-active workspace summary, derived from auth + the list. */
	get active(): WorkspaceSummary | null {
		const id = auth.session?.user.workspaceId;
		if (!id) return null;
		return this.#workspaces.find((w) => w.id === id) ?? null;
	}

	/** Idempotent: safe to call from the layout on every navigation. */
	async load(): Promise<void> {
		if (this.#loaded || this.#loading) return;
		this.#loading = true;
		try {
			this.#workspaces = await listWorkspaces();
			this.#loaded = true;
		} catch {
			// Quiet failure — picker stays empty, navigation continues.
			this.#workspaces = [];
		} finally {
			this.#loading = false;
		}
	}

	/** Force-refetch — call after membership mutations (add/remove member). */
	async refresh(): Promise<void> {
		this.#loaded = false;
		await this.load();
	}

	/**
	 * Switch to a different workspace. Persists via the server-side cookie
	 * then forces a hard reload so every cached store (templates list,
	 * projects, etc.) refetches under the new workspace_id.
	 */
	async switchTo(workspaceId: string): Promise<void> {
		await setActiveWorkspaceApi(workspaceId);
		// Hard reload to flush every in-memory store keyed by workspace_id.
		// Cheaper than wiring a workspace-aware invalidator into each one.
		if (typeof window !== 'undefined') {
			window.location.reload();
		}
	}
}

export const workspaces = new WorkspaceStore();
