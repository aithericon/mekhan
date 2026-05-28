<script lang="ts">
	import * as Command from '$lib/components/ui/command';
	import FolderKanban from '@lucide/svelte/icons/folder-kanban';
	import Check from '@lucide/svelte/icons/check';
	import LoaderCircle from '@lucide/svelte/icons/loader-circle';
	import { listProjects, attachTemplateToProject, type Project } from '$lib/api/client';
	import { workspaces } from '$lib/workspaces/store.svelte';

	interface Props {
		open: boolean;
		templateId: string | null;
		templateName?: string;
	}

	let { open = $bindable(), templateId, templateName }: Props = $props();

	let projects = $state<Project[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let attachingId = $state<string | null>(null);
	let attachedIds = $state<Set<string>>(new Set());

	async function loadProjects() {
		const ws = workspaces.active?.id;
		if (!ws) {
			projects = [];
			return;
		}
		loading = true;
		error = null;
		try {
			projects = await listProjects(ws);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load projects';
			projects = [];
		} finally {
			loading = false;
		}
	}

	// Reload + reset transient state each time the dialog opens.
	$effect(() => {
		if (open) {
			attachedIds = new Set();
			error = null;
			loadProjects();
		}
	});

	async function assign(project: Project) {
		if (!templateId || attachingId) return;
		attachingId = project.id;
		error = null;
		try {
			await attachTemplateToProject(project.id, templateId);
			attachedIds = new Set([...attachedIds, project.id]);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to assign to project';
		} finally {
			attachingId = null;
		}
	}
</script>

<Command.Dialog
	bind:open
	title="Assign to project"
	description={`Search projects and add ${templateName ?? 'this template'}.`}
	class="max-w-md"
>
	<Command.Input placeholder="Search projects…" data-testid="assign-project-search" />
	<Command.List>
		{#if loading}
			<div class="py-6 text-center text-sm text-muted-foreground">Loading projects…</div>
		{:else if !workspaces.active}
			<div class="py-6 text-center text-sm text-muted-foreground">No active workspace.</div>
		{:else}
			<Command.Empty>No projects found. Create one in Manage projects.</Command.Empty>
			<Command.Group heading="Projects">
				{#each projects as p (p.id)}
					<Command.Item
						value={p.id}
						keywords={[p.display_name, p.slug]}
						onSelect={() => assign(p)}
						data-testid={`assign-project-${p.slug}`}
					>
						<FolderKanban class="size-4 text-muted-foreground" />
						<span class="flex-1 truncate">{p.display_name}</span>
						{#if attachingId === p.id}
							<LoaderCircle class="size-4 animate-spin text-muted-foreground" />
						{:else if attachedIds.has(p.id)}
							<Check class="size-4 text-green-600" data-testid={`assign-project-done-${p.slug}`} />
						{/if}
					</Command.Item>
				{/each}
			</Command.Group>
		{/if}
		{#if error}
			<div class="px-3 py-2 text-sm text-destructive" data-testid="assign-project-error">
				{error}
			</div>
		{/if}
	</Command.List>
</Command.Dialog>
