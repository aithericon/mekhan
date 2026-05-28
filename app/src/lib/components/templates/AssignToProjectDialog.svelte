<script lang="ts">
	import * as Command from '$lib/components/ui/command';
	import FolderKanban from '@lucide/svelte/icons/folder-kanban';
	import Check from '@lucide/svelte/icons/check';
	import LoaderCircle from '@lucide/svelte/icons/loader-circle';
	import {
		listProjects,
		listTemplateProjects,
		attachTemplateToProject,
		detachTemplateFromProject,
		type Project
	} from '$lib/api/client';
	import { workspaces } from '$lib/workspaces/store.svelte';

	interface Props {
		open: boolean;
		/** Version id — used to attach and to look up current membership. */
		templateId: string | null;
		/** Chain-root id — required to detach (project_templates keys on it). */
		baseTemplateId: string | null;
		templateName?: string;
	}

	let { open = $bindable(), templateId, baseTemplateId, templateName }: Props = $props();

	let projects = $state<Project[]>([]);
	let assignedIds = $state<Set<string>>(new Set());
	let loading = $state(false);
	let error = $state<string | null>(null);
	let busyId = $state<string | null>(null);

	async function load() {
		const ws = workspaces.active?.id;
		if (!ws || !templateId) {
			projects = [];
			assignedIds = new Set();
			return;
		}
		loading = true;
		error = null;
		try {
			const [all, mine] = await Promise.all([
				listProjects(ws),
				listTemplateProjects(templateId)
			]);
			projects = all;
			assignedIds = new Set(mine.map((p) => p.id));
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load projects';
			projects = [];
			assignedIds = new Set();
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (open) load();
	});

	async function toggle(project: Project) {
		if (!templateId || busyId) return;
		const isAssigned = assignedIds.has(project.id);
		if (isAssigned && !baseTemplateId) {
			error = 'Cannot determine base template to remove from project';
			return;
		}
		busyId = project.id;
		error = null;
		try {
			if (isAssigned) {
				await detachTemplateFromProject(project.id, baseTemplateId!);
				const next = new Set(assignedIds);
				next.delete(project.id);
				assignedIds = next;
			} else {
				await attachTemplateToProject(project.id, templateId);
				assignedIds = new Set([...assignedIds, project.id]);
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to update project';
		} finally {
			busyId = null;
		}
	}
</script>

<Command.Dialog
	bind:open
	title="Assign to project"
	description={`Add or remove ${templateName ?? 'this template'} from projects.`}
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
					{@const assigned = assignedIds.has(p.id)}
					<Command.Item
						value={p.id}
						keywords={[p.display_name, p.slug]}
						onSelect={() => toggle(p)}
						data-testid={`assign-project-${p.slug}`}
					>
						<FolderKanban class="size-4 text-muted-foreground" />
						<span class="flex-1 truncate">{p.display_name}</span>
						{#if busyId === p.id}
							<LoaderCircle class="size-4 animate-spin text-muted-foreground" />
						{:else if assigned}
							<span class="flex items-center gap-1 text-xs text-green-600" data-testid={`assign-project-assigned-${p.slug}`}>
								<Check class="size-4" />
								Added
							</span>
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
