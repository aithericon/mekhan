import { getContext, setContext } from 'svelte';
import type { Project } from '$lib/api/client';

/**
 * Shared reactive handle for the project detail layout and its tab subroutes
 * (templates / api / settings). The layout owns a single `$state` object and
 * provides it here; subpages read `project`/`loading`/`error` reactively and
 * call `reload()` after mutations. Mirrors the instance-context pattern.
 */
export interface ProjectContext {
	projectId: string;
	/** Owning workspace of the loaded project (source of truth for bundle URLs). */
	workspaceId: string;
	project: Project | null;
	loading: boolean;
	error: string | null;
	reload: () => Promise<void>;
}

const KEY = Symbol('project-context');

export function provideProjectContext(ctx: ProjectContext): void {
	setContext(KEY, ctx);
}

export function getProjectContext(): ProjectContext {
	return getContext(KEY);
}
