<script lang="ts">
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import UserPlus from '@lucide/svelte/icons/user-plus';
	import Copy from '@lucide/svelte/icons/copy';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import BookOpen from '@lucide/svelte/icons/book-open';
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
	import {
		getWorkspace,
		listWorkspaceMembers,
		addWorkspaceMember,
		removeWorkspaceMember,
		listProjects,
		createProject,
		resolveUserByEmail,
		type WorkspaceSummary,
		type WorkspaceMember,
		type Project
	} from '$lib/api/client';

	const workspaceId = $derived($page.params.id ?? '');

	let workspace = $state<WorkspaceSummary | null>(null);
	let members = $state<WorkspaceMember[]>([]);
	let projects = $state<Project[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Add-member form state
	let newMemberEmail = $state('');
	let newMemberRole = $state<'viewer' | 'editor' | 'admin' | 'owner'>('editor');
	let addingMember = $state(false);
	let addError = $state<string | null>(null);

	// New-project form state
	let newProjectSlug = $state('');
	let newProjectName = $state('');
	let creatingProject = $state(false);
	let projectError = $state<string | null>(null);

	async function load() {
		loading = true;
		error = null;
		try {
			[workspace, members, projects] = await Promise.all([
				getWorkspace(workspaceId),
				listWorkspaceMembers(workspaceId),
				listProjects(workspaceId)
			]);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load workspace';
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (workspaceId) load();
	});

	async function addMember(e: Event) {
		e.preventDefault();
		const email = newMemberEmail.trim();
		if (!email) return;
		addingMember = true;
		addError = null;
		try {
			const resolved = await resolveUserByEmail(email);
			await addWorkspaceMember(workspaceId, {
				subject: resolved.subject,
				role: newMemberRole
			});
			newMemberEmail = '';
			members = await listWorkspaceMembers(workspaceId);
		} catch (e) {
			addError = e instanceof Error ? e.message : 'Failed to add member';
		} finally {
			addingMember = false;
		}
	}

	async function removeMember(userId: string) {
		if (!confirm('Remove this member?')) return;
		try {
			await removeWorkspaceMember(workspaceId, userId);
			members = members.filter((m) => m.user_id !== userId);
		} catch (e) {
			alert(e instanceof Error ? e.message : 'Failed to remove member');
		}
	}

	async function handleCreateProject(e: Event) {
		e.preventDefault();
		const slug = newProjectSlug.trim();
		const display = newProjectName.trim() || slug;
		if (!slug) return;
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
		return `/api/v1/workspaces/${workspaceId}/projects/${p.id}/openapi.json`;
	}

	async function copyBundleUrl(p: Project) {
		const url = `${window.location.origin}${bundleUrl(p)}`;
		try {
			await navigator.clipboard.writeText(url);
		} catch {
			// Fallback for browsers blocking clipboard write
			prompt('Copy this URL', url);
		}
	}
</script>

<div class="mx-auto max-w-4xl px-6 py-8" data-testid="workspace-detail">
	{#if loading}
		<div class="text-sm text-muted-foreground">Loading workspace…</div>
	{:else if error}
		<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{:else if workspace}
		<header class="mb-8 flex items-baseline justify-between">
			<div>
				<h1 class="text-2xl font-semibold tracking-tight">{workspace.display_name}</h1>
				<p class="text-sm text-muted-foreground">
					{workspace.slug} · {members.length}
					{members.length === 1 ? 'member' : 'members'}
				</p>
			</div>
			{#if workspace.is_system}
				<Badge variant="secondary">system</Badge>
			{/if}
		</header>

		<div class="grid gap-6 md:grid-cols-2">
			<!-- Members -->
			<Card data-testid="members-card">
				<CardHeader>
					<CardTitle>Members</CardTitle>
					<CardDescription>
						Owners and admins can add/remove members. Workspace can never be
						left without an owner.
					</CardDescription>
				</CardHeader>
				<CardContent class="space-y-4">
					<form onsubmit={addMember} class="space-y-2">
						<div class="flex gap-2">
							<Input
								type="email"
								placeholder="email@corp.com"
								bind:value={newMemberEmail}
								data-testid="input-new-member-email"
								class="flex-1"
							/>
							<select
								bind:value={newMemberRole}
								class="rounded-md border border-input bg-background px-2 text-sm"
								data-testid="select-new-member-role"
							>
								<option value="viewer">Viewer</option>
								<option value="editor">Editor</option>
								<option value="admin">Admin</option>
								<option value="owner">Owner</option>
							</select>
							<Button type="submit" disabled={addingMember} data-testid="btn-add-member">
								<UserPlus class="size-4" />
								Add
							</Button>
						</div>
						{#if addError}
							<div class="text-xs text-destructive">{addError}</div>
						{/if}
					</form>

					<ul class="divide-y divide-border rounded-md border border-border">
						{#each members as m (m.user_id)}
							<li
								class="flex items-center justify-between gap-3 px-3 py-2 text-sm"
								data-testid={`member-row-${m.user_id}`}
							>
								<div class="min-w-0 flex-1">
									<div class="truncate font-mono text-xs text-muted-foreground">{m.user_id}</div>
								</div>
								<Badge variant="secondary">{m.role}</Badge>
								<button
									type="button"
									class="text-muted-foreground hover:text-destructive"
									onclick={() => removeMember(m.user_id)}
									data-testid={`btn-remove-member-${m.user_id}`}
									aria-label="Remove member"
								>
									<Trash2 class="size-3.5" />
								</button>
							</li>
						{/each}
					</ul>
				</CardContent>
			</Card>

			<!-- Projects -->
			<Card data-testid="projects-card">
				<CardHeader>
					<CardTitle>Projects</CardTitle>
					<CardDescription>
						Group related templates. Each project gets its own per-webhook
						OpenAPI bundle.
					</CardDescription>
				</CardHeader>
				<CardContent class="space-y-4">
					<form onsubmit={handleCreateProject} class="space-y-2">
						<div class="grid gap-2 sm:grid-cols-2">
							<Input
								placeholder="slug"
								bind:value={newProjectSlug}
								data-testid="input-new-project-slug"
							/>
							<Input
								placeholder="Display name"
								bind:value={newProjectName}
								data-testid="input-new-project-name"
							/>
						</div>
						<Button
							type="submit"
							disabled={creatingProject}
							class="w-full"
							data-testid="btn-create-project"
						>
							<Plus class="size-4" />
							{creatingProject ? 'Creating…' : 'Create project'}
						</Button>
						{#if projectError}
							<div class="text-xs text-destructive">{projectError}</div>
						{/if}
					</form>

					<ul class="space-y-2">
						{#each projects as p (p.id)}
							<li
								class="rounded-md border border-border bg-card/50 p-3 text-sm"
								data-testid={`project-row-${p.slug}`}
							>
								<div class="flex items-start justify-between gap-2">
									<a
										href={`/workspaces/${workspaceId}/projects/${p.id}`}
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
											onclick={() => goto(`/workspaces/${workspaceId}/projects/${p.id}`)}
											data-testid={`btn-api-${p.slug}`}
										>
											<BookOpen class="size-3.5" />
											API
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
							<li class="rounded-md border border-dashed border-border p-3 text-center text-xs text-muted-foreground">
								No projects yet
							</li>
						{/each}
					</ul>
				</CardContent>
			</Card>
		</div>
	{/if}
</div>

