<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import Settings from '@lucide/svelte/icons/settings';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import * as Tabs from '$lib/components/ui/tabs';
	import { workspaces } from '$lib/workspaces/store.svelte';
	import { listProjects } from '$lib/api/client';
	import {
		provideProjectContext,
		type ProjectContext
	} from '$lib/components/projects/project-context';

	let { children } = $props();

	const projectId = $derived(page.params.projectId ?? '');
	const pathname = $derived(page.url.pathname);

	// Single reactive handle shared with every subroute. Subpages mutate
	// `project` via `reload()` (or by assigning after a save); the object
	// reference itself stays stable.
	const ctx = $state<ProjectContext>({
		projectId: '', // set from the route in $effect below
		workspaceId: '',
		project: null,
		loading: true,
		error: null,
		reload
	});

	provideProjectContext(ctx);

	async function reload() {
		ctx.loading = true;
		ctx.error = null;
		try {
			await workspaces.load();
			const wsId = workspaces.active?.id ?? '';
			ctx.workspaceId = wsId;
			if (!wsId) {
				ctx.error = 'No active workspace. Pick one from the workspace switcher first.';
				return;
			}
			// Projects are scoped to the active workspace (same as the list page).
			const projects = await listProjects(wsId);
			ctx.project = projects.find((p) => p.id === ctx.projectId) ?? null;
			if (!ctx.project) {
				ctx.error = 'Project not found in the active workspace';
			} else {
				ctx.workspaceId = ctx.project.workspace_id;
			}
		} catch (e) {
			ctx.error = e instanceof Error ? e.message : 'Failed to load project';
		} finally {
			ctx.loading = false;
		}
	}

	$effect(() => {
		ctx.projectId = projectId;
		if (projectId) reload();
	});

	// Tab nav drives the subroutes. Settings lives behind the gear, so the
	// active tab is empty there (no trigger highlighted).
	const activeTab = $derived(pathname.endsWith('/api') ? 'api' : pathname.endsWith('/settings') ? '' : 'templates');
	const onSettings = $derived(pathname.endsWith('/settings'));

	function selectTab(v: string | undefined) {
		if (v && v !== activeTab) goto(`/projects/${ctx.projectId}/${v}`);
	}
</script>

<svelte:head><title>{ctx.project?.display_name ?? 'Project'} | Mekhan</title></svelte:head>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-4xl px-6 py-8">
		<a
			href="/projects"
			class="mb-3 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
		>
			<ChevronLeft class="size-4" /> Projects
		</a>

		{#if ctx.loading && !ctx.project}
			<p class="text-sm text-muted-foreground">Loading…</p>
		{:else if ctx.error && !ctx.project}
			<div class="rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
				{ctx.error}
			</div>
		{:else if ctx.project}
			<!-- Static project header — shared across every tab subroute. -->
			<div class="mb-5 flex items-start justify-between gap-3">
				<div class="min-w-0">
					<h1 class="text-lg font-semibold tracking-tight">{ctx.project.display_name}</h1>
					<div class="mt-1 flex items-center gap-2">
						<Badge variant="secondary" class="font-mono text-sm">{ctx.project.slug}</Badge>
						{#if ctx.project.description}
							<span class="truncate text-sm text-muted-foreground">{ctx.project.description}</span>
						{/if}
					</div>
				</div>
				<Button
					variant={onSettings ? 'secondary' : 'ghost'}
					size="sm"
					title="Project settings"
					onclick={() => goto(`/projects/${ctx.projectId}/settings`)}
					data-testid="btn-project-settings"
				>
					<Settings class="size-4" /> Settings
				</Button>
			</div>

			<Tabs.Root value={activeTab} onValueChange={selectTab} class="mb-5">
				<Tabs.List>
					<Tabs.Trigger value="templates" data-testid="tab-templates">Templates</Tabs.Trigger>
					<Tabs.Trigger value="api" data-testid="tab-api">API</Tabs.Trigger>
				</Tabs.List>
			</Tabs.Root>

			{@render children()}
		{/if}
	</div>
</div>
