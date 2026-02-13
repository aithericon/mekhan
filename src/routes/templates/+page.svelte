<script lang="ts">
	import { goto } from '$app/navigation';
	import { listTemplates, createTemplate, deleteTemplate } from '$lib/api/client';
	import type { TemplateSummary } from '$lib/types/api';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import FileText from '@lucide/svelte/icons/file-text';

	let templates = $state<TemplateSummary[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	async function load() {
		loading = true;
		error = null;
		try {
			const result = await listTemplates();
			templates = result.items;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load templates';
			// For now, show empty state when API is not available
			templates = [];
		} finally {
			loading = false;
		}
	}

	async function handleCreate() {
		try {
			const template = await createTemplate({
				name: 'Untitled Workflow',
				description: '',
				author_id: 'default-user'
			});
			goto(`/templates/${template.id}`);
		} catch (e) {
			// If API not available, navigate with a mock ID for development
			goto(`/templates/new`);
		}
	}

	async function handleDelete(id: string) {
		if (!confirm('Delete this template?')) return;
		try {
			await deleteTemplate(id);
			templates = templates.filter((t) => t.id !== id);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to delete';
		}
	}

	const formatDate = (s: string) => new Date(s).toLocaleDateString();

	// Load on mount
	$effect(() => {
		load();
	});
</script>

<div class="h-full overflow-y-auto" data-testid="templates-page">
	<div class="mx-auto max-w-5xl px-6 py-8">
		<div class="mb-6 flex items-center justify-between">
			<div>
				<h1 class="text-2xl font-semibold tracking-tight text-foreground">Templates</h1>
				<p class="mt-1 text-sm text-muted-foreground">
					Create and manage workflow templates
				</p>
			</div>
			<button
				type="button"
				class="flex items-center gap-2 rounded-lg bg-primary px-4 py-2 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90"
				data-testid="btn-create-template"
				onclick={handleCreate}
			>
				<Plus class="size-4" />
				New Template
			</button>
		</div>

		{#if error}
			<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
				{error}
			</div>
		{/if}

		{#if loading}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Loading...
			</div>
		{:else if templates.length === 0}
			<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
				<FileText class="size-10 text-muted-foreground/40" />
				<p class="mt-3 text-sm text-muted-foreground">No templates yet</p>
				<button
					type="button"
					class="mt-3 flex items-center gap-2 rounded-lg bg-primary px-4 py-2 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90"
					onclick={handleCreate}
				>
					<Plus class="size-4" />
					Create your first template
				</button>
			</div>
		{:else}
			<div class="space-y-2" data-testid="template-list">
				{#each templates as template (template.id)}
					<a
						href="/templates/{template.id}"
						class="group flex items-center justify-between rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent/50"
						data-testid="template-item-{template.id}"
					>
						<div class="min-w-0">
							<div class="flex items-center gap-2">
								<span class="text-sm font-medium text-foreground">{template.name}</span>
								<span class="rounded-full px-2 py-0.5 text-[10px] font-medium {template.published ? 'bg-green-100 text-green-700' : 'bg-amber-100 text-amber-700'}">
									{template.published ? 'Published' : 'Draft'} v{template.version}
								</span>
							</div>
							{#if template.description}
								<p class="mt-1 truncate text-xs text-muted-foreground">{template.description}</p>
							{/if}
							<p class="mt-1 text-[10px] text-muted-foreground">
								Updated {formatDate(template.updated_at)}
							</p>
						</div>
						<button
							type="button"
							class="rounded p-1.5 text-muted-foreground opacity-0 transition-all hover:bg-destructive/10 hover:text-destructive group-hover:opacity-100"
							data-testid="btn-delete-template-{template.id}"
							onclick={(e) => {
								e.preventDefault();
								e.stopPropagation();
								handleDelete(template.id);
							}}
						>
							<Trash2 class="size-4" />
						</button>
					</a>
				{/each}
			</div>
		{/if}
	</div>
</div>
