<script lang="ts">
	import { goto } from '$app/navigation';
	import Plus from '@lucide/svelte/icons/plus';
	import Copy from '@lucide/svelte/icons/copy';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import BookOpen from '@lucide/svelte/icons/book-open';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { workspaces } from '$lib/workspaces/store.svelte';
	import { listProjects, createProject, type Project } from '$lib/api/client';

	// Top-level projects are scoped to the active workspace — same implicit
	// scoping every other top-level page (Templates, Instances, …) uses.
	// Switch workspaces from the picker to see another workspace's projects.
	const workspaceId = $derived(workspaces.active?.id ?? '');

	let projects = $state<Project[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// New-project form state
	let newProjectSlug = $state('');
	let newProjectName = $state('');
	let creatingProject = $state(false);
	let projectError = $state<string | null>(null);

	async function load() {
		if (!workspaceId) return;
		loading = true;
		error = null;
		try {
			projects = await listProjects(workspaceId);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load projects';
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		// Ensure the workspace list (and thus `active`) is hydrated, then load.
		workspaces.load();
		if (workspaceId) load();
	});

	async function handleCreateProject(e: Event) {
		e.preventDefault();
		const slug = newProjectSlug.trim();
		const display = newProjectName.trim() || slug;
		if (!slug || !workspaceId) return;
		creatingProject = true;
		projectError = null;
		try {
			const p = await createProject(workspaceId, {
				slug,
				display_name: display,
				description: ''
			});
			projects = [...projects, p];
			newProjectSlug = '';
			newProjectName = '';
		} catch (e) {
			projectError = e instanceof Error ? e.message : 'Failed to create project';
		} finally {
			creatingProject = false;
		}
	}

	function bundleUrl(p: Project): string {
		return `/api/v1/workspaces/${p.workspace_id}/projects/${p.id}/openapi.json`;
	}

	async function copyBundleUrl(p: Project) {
		const url = `${window.location.origin}${bundleUrl(p)}`;
		try {
			await navigator.clipboard.writeText(url);
		} catch {
			prompt('Copy this URL', url);
		}
	}
</script>

<svelte:head><title>Projects | Mekhan</title></svelte:head>

<div class="mx-auto max-w-4xl px-6 py-8" data-testid="projects-index">
	<header class="mb-6 flex items-baseline justify-between">
		<div>
			<h1 class="text-2xl font-semibold tracking-tight">Projects</h1>
			<p class="mt-1 text-sm text-muted-foreground">
				Group related templates. Each project gets its own per-webhook OpenAPI
				bundle.{#if workspaces.active}
					Workspace: <span class="font-medium">{workspaces.active.display_name}</span>.{/if}
			</p>
		</div>
	</header>

	{#if !workspaceId}
		<div class="rounded-lg border border-dashed border-border p-6 text-sm text-muted-foreground">
			No active workspace. Pick one from the workspace switcher first.
		</div>
	{:else}
		<form
			onsubmit={handleCreateProject}
			class="mb-6 flex flex-wrap items-end gap-2 rounded-lg border border-border bg-card/50 p-4"
		>
			<label class="flex-1 space-y-1 text-sm">
				<span class="text-muted-foreground">Slug</span>
				<Input placeholder="slug" bind:value={newProjectSlug} data-testid="input-new-project-slug" />
			</label>
			<label class="flex-1 space-y-1 text-sm">
				<span class="text-muted-foreground">Display name</span>
				<Input
					placeholder="Display name"
					bind:value={newProjectName}
					data-testid="input-new-project-name"
				/>
			</label>
			<Button type="submit" disabled={creatingProject} data-testid="btn-create-project">
				<Plus class="size-4" />
				{creatingProject ? 'Creating…' : 'Create project'}
			</Button>
		</form>
		{#if projectError}
			<div class="mb-4 text-xs text-destructive">{projectError}</div>
		{/if}

		{#if loading}
			<p class="text-sm text-muted-foreground">Loading…</p>
		{:else if error}
			<div class="rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
				{error}
			</div>
		{:else}
			<ul class="space-y-2" data-testid="projects-list">
				{#each projects as p (p.id)}
					<li
						class="rounded-md border border-border bg-card/50 p-3 text-sm"
						data-testid={`project-row-${p.slug}`}
					>
						<div class="flex items-start justify-between gap-2">
							<a
								href={`/projects/${p.id}`}
								class="min-w-0 flex-1"
								data-testid={`link-project-${p.slug}`}
							>
								<div class="font-medium hover:underline">{p.display_name}</div>
								<div class="truncate text-xs text-muted-foreground">{p.slug}</div>
							</a>
							<div class="flex gap-1">
								<Button
									variant="outline"
									size="sm"
									title="Open project — API contract, templates, settings"
									onclick={() => goto(`/projects/${p.id}`)}
									data-testid={`btn-api-${p.slug}`}
								>
									<BookOpen class="size-3.5" />
									Open
								</Button>
								<Button
									variant="ghost"
									size="sm"
									title="Copy OpenAPI bundle URL"
									onclick={() => copyBundleUrl(p)}
									data-testid={`btn-copy-bundle-${p.slug}`}
								>
									<Copy class="size-3.5" />
								</Button>
								<a
									href={bundleUrl(p)}
									target="_blank"
									rel="noopener"
									class="inline-flex h-8 items-center justify-center rounded-md px-2 text-muted-foreground hover:bg-accent hover:text-foreground"
									title="Open OpenAPI bundle in new tab"
									data-testid={`link-open-bundle-${p.slug}`}
								>
									<ExternalLink class="size-3.5" />
								</a>
							</div>
						</div>
					</li>
				{:else}
					<li class="rounded-md border border-dashed border-border p-6 text-center text-sm text-muted-foreground">
						No projects yet. Create one above.
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</div>
