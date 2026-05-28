<script lang="ts">
	import { onMount } from 'svelte';
	import FolderKanban from '@lucide/svelte/icons/folder-kanban';
	import Tag from '@lucide/svelte/icons/tag';
	import X from '@lucide/svelte/icons/x';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { listProjects, listWorkspaceTags, type Project } from '$lib/api/client';
	import { workspaces } from '$lib/workspaces/store.svelte';

	interface Props {
		projectId: string | null;
		tag: string | null;
		onChange: (next: { projectId: string | null; tag: string | null }) => void;
	}

	let { projectId, tag, onChange }: Props = $props();

	let projects = $state<Project[]>([]);
	let tags = $state<string[]>([]);
	let loading = $state(false);

	async function loadFilters(workspaceId: string) {
		loading = true;
		try {
			projects = await listProjects(workspaceId);
		} catch {
			projects = [];
		}
		try {
			tags = await listWorkspaceTags(workspaceId);
		} catch {
			tags = [];
		}
		loading = false;
	}

	$effect(() => {
		const ws = workspaces.active?.id;
		if (ws) loadFilters(ws);
	});

	function selectProject(id: string | null) {
		onChange({ projectId: id, tag });
	}

	function selectTag(t: string | null) {
		onChange({ projectId, tag: t });
	}

	function clearFilters() {
		onChange({ projectId: null, tag: null });
	}

	const hasFilters = $derived(projectId !== null || tag !== null);
</script>

<aside class="w-60 shrink-0 border-r border-border bg-card/30" data-testid="templates-filters-sidebar">
	<div class="space-y-6 p-4">
		{#if hasFilters}
			<Button
				variant="ghost"
				size="sm"
				class="w-full justify-start text-muted-foreground"
				onclick={clearFilters}
				data-testid="btn-clear-filters"
			>
				<X class="size-3.5" />
				Clear filters
			</Button>
		{/if}

		<section>
			<div class="mb-2 flex items-center gap-2 text-sm font-medium text-foreground">
				<FolderKanban class="size-4 text-muted-foreground" />
				Projects
			</div>
			<ul class="space-y-0.5">
				<li>
					<button
						type="button"
						class="w-full rounded px-2 py-1 text-left text-sm hover:bg-accent {projectId === null ? 'bg-accent text-foreground font-medium' : 'text-muted-foreground'}"
						onclick={() => selectProject(null)}
						data-testid="filter-project-all"
					>
						All templates
					</button>
				</li>
				{#each projects as p (p.id)}
					<li>
						<button
							type="button"
							class="w-full truncate rounded px-2 py-1 text-left text-sm hover:bg-accent {projectId === p.id ? 'bg-accent text-foreground font-medium' : 'text-muted-foreground'}"
							onclick={() => selectProject(p.id)}
							data-testid={`filter-project-${p.slug}`}
							title={p.description || p.display_name}
						>
							{p.display_name}
						</button>
					</li>
				{:else}
					<li class="px-2 py-1 text-sm text-muted-foreground/60 italic">
						No projects yet
					</li>
				{/each}
			</ul>
		</section>

		{#if tags.length > 0}
			<section>
				<div class="mb-2 flex items-center gap-2 text-sm font-medium text-foreground">
					<Tag class="size-4 text-muted-foreground" />
					Tags
				</div>
				<div class="flex flex-wrap gap-1" data-testid="filter-tags">
					{#each tags as t (t)}
						<button
							type="button"
							class="rounded border px-2 py-0.5 text-sm transition-colors {tag === t ? 'border-foreground bg-foreground text-background' : 'border-border text-muted-foreground hover:bg-accent'}"
							onclick={() => selectTag(tag === t ? null : t)}
							data-testid={`filter-tag-${t}`}
						>
							{t}
						</button>
					{/each}
				</div>
			</section>
		{/if}

		{#if loading}
			<div class="text-sm text-muted-foreground">Loading filters…</div>
		{/if}
	</div>
</aside>
