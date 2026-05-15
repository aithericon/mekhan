<script lang="ts">
	import { goto } from '$app/navigation';
	import {
		listTemplates,
		createTemplate,
		deleteTemplate,
		updateTemplate,
		createNewVersion,
		createInstance,
		type TemplateSummary
	} from '$lib/api/client';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { Input } from '$lib/components/ui/input';
	import {
		DropdownMenu,
		DropdownMenuTrigger,
		DropdownMenuContent,
		DropdownMenuItem,
		DropdownMenuSeparator
	} from '$lib/components/ui/dropdown-menu';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import FileText from '@lucide/svelte/icons/file-text';
	import Rocket from '@lucide/svelte/icons/rocket';
	import Pencil from '@lucide/svelte/icons/pencil';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import EllipsisVertical from '@lucide/svelte/icons/ellipsis-vertical';
	import CreateInstanceDialog from '$lib/components/instances/CreateInstanceDialog.svelte';

	let templates = $state<TemplateSummary[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let dialogOpen = $state(false);
	let dialogTemplateId = $state<string | null>(null);

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
				description: ''
			});
			goto(`/templates/${template.id}`);
		} catch (e) {
			// If API not available, navigate with a mock ID for development
			goto(`/templates/new`);
		}
	}

	// `new-version` is server-side only valid on a published template: it forks
	// a fresh draft (version+1, is_latest) and marks the source not-latest.
	let versioningId = $state<string | null>(null);
	async function handleNewVersion(id: string) {
		if (versioningId) return;
		versioningId = id;
		try {
			const next = await createNewVersion(id);
			goto(`/templates/${next.id}`);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to create new version';
			versioningId = null;
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

	// Inline rename (draft-only — published templates are server-locked).
	let renamingId = $state<string | null>(null);
	let renameDraft = $state('');
	let renameRef = $state<HTMLInputElement | null>(null);

	function startRename(t: TemplateSummary) {
		renamingId = t.id;
		renameDraft = t.name;
	}

	async function commitRename() {
		const id = renamingId;
		if (!id) return;
		renamingId = null;
		const t = templates.find((x) => x.id === id);
		if (!t) return;
		const next = renameDraft.trim();
		if (!next || next === t.name) return;
		const prev = t.name;
		templates = templates.map((x) => (x.id === id ? { ...x, name: next } : x)); // optimistic
		try {
			await updateTemplate(id, { name: next });
		} catch (e) {
			templates = templates.map((x) => (x.id === id ? { ...x, name: prev } : x));
			error = e instanceof Error ? e.message : 'Rename failed';
		}
	}

	function onRenameKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') {
			e.preventDefault();
			commitRename();
		} else if (e.key === 'Escape') {
			e.preventDefault();
			renamingId = null;
		}
	}

	$effect(() => {
		if (renamingId) renameRef?.focus();
	});

	function handleCreateInstance(templateId: string) {
		// Open the dialog. It loads the template, inspects each Start block's
		// `initial` port, and either prompts for tokens or POSTs immediately when
		// no typed fields are declared. Direct call no longer hits the API here.
		dialogTemplateId = templateId;
		dialogOpen = true;
	}

	function onInstanceCreated(instanceId: string) {
		dialogOpen = false;
		dialogTemplateId = null;
		goto(`/instances/${instanceId}`);
	}

	// Clear the stashed template id once the dialog closes (whatever path —
	// X button, Cancel, or successful submit). This keeps the dialog inert
	// after dismissal so opening it again starts fresh.
	$effect(() => {
		if (!dialogOpen) dialogTemplateId = null;
	});

	const formatDate = (s: string) => new Date(s).toLocaleDateString();

	// Load on mount
	$effect(() => {
		load();
	});
</script>

<div class="h-full overflow-y-auto" data-testid="templates-page">
	<div class="mx-auto max-w-5xl px-6 py-8 animate-rise">
		<div class="mb-6 flex items-center justify-between">
			<div>
				<h1 class="text-2xl font-semibold tracking-tight text-foreground">Templates</h1>
				<p class="mt-1 text-sm text-muted-foreground">
					Create and manage workflow templates
				</p>
			</div>
			<Button data-testid="btn-create-template" onclick={handleCreate}>
				<Plus class="size-4" />
				New Template
			</Button>
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
				<Button class="mt-3" onclick={handleCreate}>
					<Plus class="size-4" />
					Create your first template
				</Button>
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
								{#if renamingId === template.id}
									<Input
										bind:ref={renameRef}
										bind:value={renameDraft}
										onkeydown={onRenameKeydown}
										onblur={commitRename}
										onclick={(e: MouseEvent) => {
											e.preventDefault();
											e.stopPropagation();
										}}
										onfocus={(e) => (e.currentTarget as HTMLInputElement).select()}
										aria-label="Template name"
										data-testid="input-rename-template-{template.id}"
										class="h-7 w-64 text-sm font-medium"
									/>
								{:else}
									<span class="text-sm font-medium text-foreground">{template.name}</span>
								{/if}
								<Badge class={template.published ? 'bg-green-100 text-green-700' : 'bg-amber-100 text-amber-700'} variant="secondary">
									{template.published ? 'Published' : 'Draft'} v{template.version}
								</Badge>
							</div>
							{#if template.description}
								<p class="mt-1 truncate text-xs text-muted-foreground">{template.description}</p>
							{/if}
							<p class="mt-1 text-[10px] text-muted-foreground">
								Updated {formatDate(template.updated_at)}
							</p>
						</div>
						<div class="flex items-center gap-1">
							{#if template.published}
								<Button
									variant="ghost"
									size="icon-sm"
									class="text-muted-foreground opacity-0 transition-all hover:bg-primary/10 hover:text-primary group-hover:opacity-100"
									data-testid="btn-run-template-{template.id}"
									onclick={(e: MouseEvent) => {
										e.preventDefault();
										e.stopPropagation();
										handleCreateInstance(template.id);
									}}
								>
									<Rocket class="size-4" />
								</Button>
							{/if}
							<DropdownMenu>
								<DropdownMenuTrigger
									data-testid="btn-template-menu-{template.id}"
									aria-label="Template actions"
									class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground opacity-0 transition-all hover:bg-accent hover:text-foreground group-hover:opacity-100 data-[state=open]:opacity-100"
									onclick={(e: MouseEvent) => {
										e.preventDefault();
										e.stopPropagation();
									}}
								>
									<EllipsisVertical class="size-4" />
								</DropdownMenuTrigger>
								<DropdownMenuContent align="end">
									{#if !template.published}
										<DropdownMenuItem
											data-testid="btn-rename-template-{template.id}"
											onSelect={() => startRename(template)}
										>
											<Pencil class="size-4" />
											Rename
										</DropdownMenuItem>
										<DropdownMenuSeparator />
									{:else}
										<DropdownMenuItem
											data-testid="btn-new-version-template-{template.id}"
											disabled={versioningId === template.id}
											onSelect={() => handleNewVersion(template.id)}
										>
											<GitBranch class="size-4" />
											{versioningId === template.id ? 'Creating…' : 'New Version'}
										</DropdownMenuItem>
										<DropdownMenuSeparator />
									{/if}
									<DropdownMenuItem
										variant="destructive"
										data-testid="btn-delete-template-{template.id}"
										onSelect={() => handleDelete(template.id)}
									>
										<Trash2 class="size-4" />
										Delete
									</DropdownMenuItem>
								</DropdownMenuContent>
							</DropdownMenu>
						</div>
					</a>
				{/each}
			</div>
		{/if}
	</div>
</div>

<CreateInstanceDialog
	bind:open={dialogOpen}
	templateId={dialogTemplateId}
	oncreated={onInstanceCreated}
/>
