<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Plus from '@lucide/svelte/icons/plus';
	import Copy from '@lucide/svelte/icons/copy';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import Save from '@lucide/svelte/icons/save';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Badge } from '$lib/components/ui/badge';
	import {
		Card,
		CardHeader,
		CardTitle,
		CardContent,
		CardDescription
	} from '$lib/components/ui/card';
	import ProjectApiContract from '$lib/components/projects/ProjectApiContract.svelte';
	import {
		getWorkspace,
		listProjects,
		updateProject,
		deleteProject,
		listTemplates,
		attachTemplateToProject,
		detachTemplateFromProject,
		type WorkspaceSummary,
		type Project,
		type Template
	} from '$lib/api/client';

	const workspaceId = $derived(page.params.id ?? '');
	const projectId = $derived(page.params.projectId ?? '');

	let workspace = $state<WorkspaceSummary | null>(null);
	let project = $state<Project | null>(null);
	let attached = $state<Template[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Settings form state
	let editName = $state('');
	let editDescription = $state('');
	let savingSettings = $state(false);
	let settingsError = $state<string | null>(null);
	const dirty = $derived(
		!!project && (editName.trim() !== project.display_name || editDescription !== project.description)
	);

	// Attach-template state
	let attachQuery = $state('');
	let attachResults = $state<Template[]>([]);
	let attachSearching = $state(false);
	let attachError = $state<string | null>(null);

	const bundleUrl = $derived(`/api/v1/workspaces/${workspaceId}/projects/${projectId}/openapi.json`);

	// Base id is what project_templates keys on (COALESCE(base_template_id, id)).
	function baseId(t: Template): string {
		return t.base_template_id ?? t.id;
	}

	async function loadAttached() {
		try {
			const res = await listTemplates({ projectId, pageSize: 100 });
			attached = res.items ?? [];
		} catch {
			attached = [];
		}
	}

	async function load() {
		loading = true;
		error = null;
		try {
			const [ws, projects] = await Promise.all([
				getWorkspace(workspaceId),
				listProjects(workspaceId)
			]);
			workspace = ws;
			project = projects.find((p) => p.id === projectId) ?? null;
			if (!project) {
				error = 'Project not found in this workspace';
				return;
			}
			editName = project.display_name;
			editDescription = project.description;
			await loadAttached();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load project';
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (workspaceId && projectId) load();
	});

	async function saveSettings(e: Event) {
		e.preventDefault();
		if (!project || !dirty) return;
		savingSettings = true;
		settingsError = null;
		try {
			project = await updateProject(projectId, {
				display_name: editName.trim() || project.display_name,
				description: editDescription
			});
			editName = project.display_name;
			editDescription = project.description;
		} catch (err) {
			settingsError = err instanceof Error ? err.message : 'Failed to save';
		} finally {
			savingSettings = false;
		}
	}

	async function handleDelete() {
		if (!project) return;
		if (!confirm(`Delete project '${project.display_name}'? This cannot be undone.`)) return;
		try {
			await deleteProject(projectId);
			goto(`/workspaces/${workspaceId}`);
		} catch (e) {
			alert(e instanceof Error ? e.message : 'Failed to delete project');
		}
	}

	const attachedBaseIds = $derived(new Set(attached.map(baseId)));

	async function runSearch() {
		const q = attachQuery.trim();
		if (!q) {
			attachResults = [];
			return;
		}
		attachSearching = true;
		attachError = null;
		try {
			const res = await listTemplates({ search: q, pageSize: 10 });
			// Hide already-attached templates from the picker.
			attachResults = (res.items ?? []).filter((t) => !attachedBaseIds.has(baseId(t)));
		} catch (e) {
			attachError = e instanceof Error ? e.message : 'Search failed';
		} finally {
			attachSearching = false;
		}
	}

	async function attach(t: Template) {
		attachError = null;
		try {
			await attachTemplateToProject(projectId, t.id);
			attachResults = attachResults.filter((x) => x.id !== t.id);
			await loadAttached();
		} catch (e) {
			attachError = e instanceof Error ? e.message : 'Failed to attach';
		}
	}

	async function detach(t: Template) {
		if (!confirm(`Detach '${t.name}' from this project?`)) return;
		try {
			await detachTemplateFromProject(projectId, baseId(t));
			attached = attached.filter((x) => x.id !== t.id);
		} catch (e) {
			alert(e instanceof Error ? e.message : 'Failed to detach');
		}
	}

	async function copyBundleUrl() {
		const url = `${window.location.origin}${bundleUrl}`;
		try {
			await navigator.clipboard.writeText(url);
		} catch {
			prompt('Copy this URL', url);
		}
	}
</script>

<svelte:head><title>{project?.display_name ?? 'Project'} | Mekhan</title></svelte:head>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-4xl px-6 py-8 animate-rise">
		<a
			href={`/workspaces/${workspaceId}`}
			class="mb-3 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
		>
			<ChevronLeft class="size-4" /> {workspace?.display_name ?? 'Workspace'}
		</a>

		{#if loading}
			<p class="text-sm text-muted-foreground">Loading…</p>
		{:else if error}
			<div class="rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
				{error}
			</div>
		{:else if project}
			<div class="mb-6 flex items-start justify-between gap-3">
				<div class="min-w-0">
					<h1 class="text-lg font-semibold tracking-tight">{project.display_name}</h1>
					<div class="mt-1 flex items-center gap-2">
						<Badge variant="secondary" class="font-mono text-xs">{project.slug}</Badge>
						{#if project.description}
							<span class="truncate text-sm text-muted-foreground">{project.description}</span>
						{/if}
					</div>
				</div>
			</div>

			<div class="space-y-6">
				<!-- Settings -->
				<Card>
					<CardHeader>
						<CardTitle>Settings</CardTitle>
						<CardDescription>Rename this project or update its description. The slug is immutable.</CardDescription>
					</CardHeader>
					<CardContent>
						<form onsubmit={saveSettings} class="space-y-3">
							<div class="grid gap-3 sm:grid-cols-2">
								<label class="space-y-1 text-sm">
									<span class="text-muted-foreground">Display name</span>
									<Input bind:value={editName} data-testid="input-project-name" />
								</label>
								<label class="space-y-1 text-sm">
									<span class="text-muted-foreground">Description</span>
									<Input bind:value={editDescription} data-testid="input-project-description" />
								</label>
							</div>
							{#if settingsError}
								<div class="text-xs text-destructive">{settingsError}</div>
							{/if}
							<div class="flex items-center justify-between">
								<Button type="submit" size="sm" disabled={!dirty || savingSettings} data-testid="btn-save-project">
									<Save class="size-4" />
									{savingSettings ? 'Saving…' : 'Save changes'}
								</Button>
								<Button
									type="button"
									variant="outline"
									size="sm"
									class="text-destructive hover:text-destructive"
									onclick={handleDelete}
									data-testid="btn-delete-project"
								>
									<Trash2 class="size-4" /> Delete project
								</Button>
							</div>
						</form>
					</CardContent>
				</Card>

				<!-- Attached templates -->
				<Card>
					<CardHeader>
						<CardTitle>Templates</CardTitle>
						<CardDescription>
							Templates attached to this project. Their enabled triggers make up the API contract below.
						</CardDescription>
					</CardHeader>
					<CardContent class="space-y-4">
						{#if attached.length === 0}
							<p class="text-sm text-muted-foreground">No templates attached yet.</p>
						{:else}
							<ul class="space-y-2">
								{#each attached as t (t.id)}
									<li
										class="flex items-center justify-between gap-2 rounded-md border border-border bg-card/50 px-3 py-2 text-sm"
										data-testid={`attached-${t.id}`}
									>
										<a href={`/templates/${t.id}`} class="min-w-0 flex-1 hover:underline">
											<span class="font-medium">{t.name}</span>
											<span class="ml-2 text-xs text-muted-foreground">v{t.version}</span>
											{#if !t.published}
												<Badge variant="outline" class="ml-2 text-xs">draft</Badge>
											{/if}
										</a>
										<Button
											variant="ghost"
											size="sm"
											title="Detach from project"
											onclick={() => detach(t)}
											data-testid={`btn-detach-${t.id}`}
										>
											<Trash2 class="size-3.5" />
										</Button>
									</li>
								{/each}
							</ul>
						{/if}

						<!-- Attach picker -->
						<div class="space-y-2 border-t border-border/60 pt-3">
							<form onsubmit={(e) => { e.preventDefault(); runSearch(); }} class="flex gap-2">
								<Input
									placeholder="Search templates to attach…"
									bind:value={attachQuery}
									data-testid="input-attach-search"
								/>
								<Button type="submit" variant="outline" size="sm" disabled={attachSearching}>
									{attachSearching ? 'Searching…' : 'Search'}
								</Button>
							</form>
							{#if attachError}
								<div class="text-xs text-destructive">{attachError}</div>
							{/if}
							{#if attachResults.length > 0}
								<ul class="space-y-1">
									{#each attachResults as t (t.id)}
										<li class="flex items-center justify-between gap-2 rounded-md border border-dashed border-border px-3 py-1.5 text-sm">
											<span class="min-w-0 truncate">
												{t.name} <span class="text-xs text-muted-foreground">v{t.version}</span>
											</span>
											<Button
												variant="ghost"
												size="sm"
												onclick={() => attach(t)}
												data-testid={`btn-attach-${t.id}`}
											>
												<Plus class="size-3.5" /> Attach
											</Button>
										</li>
									{/each}
								</ul>
							{/if}
						</div>
					</CardContent>
				</Card>

				<!-- API contract -->
				<Card>
					<CardHeader>
						<div class="flex items-start justify-between gap-2">
							<div>
								<CardTitle>API</CardTitle>
								<CardDescription>Callable trigger contract synthesized from this project's templates.</CardDescription>
							</div>
							<div class="flex gap-1">
								<Button variant="ghost" size="sm" title="Copy OpenAPI bundle URL" onclick={copyBundleUrl}>
									<Copy class="size-3.5" />
								</Button>
								<a
									href={bundleUrl}
									target="_blank"
									rel="noopener"
									class="inline-flex h-8 items-center justify-center rounded-md px-2 text-muted-foreground hover:bg-accent hover:text-foreground"
									title="Open OpenAPI bundle in new tab"
								>
									<ExternalLink class="size-3.5" />
								</a>
							</div>
						</div>
					</CardHeader>
					<CardContent>
						<ProjectApiContract {workspaceId} {projectId} />
					</CardContent>
				</Card>
			</div>
		{/if}
	</div>
</div>
