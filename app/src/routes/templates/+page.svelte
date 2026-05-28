<script lang="ts">
	import { goto } from '$app/navigation';
	import {
		listTemplates,
		createTemplate,
		deleteTemplate,
		updateTemplate,
		createNewVersion,
		createInstance,
		listInstances,
		type TemplateSummary,
		type WorkflowGraph
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
	import Activity from '@lucide/svelte/icons/activity';
	import EllipsisVertical from '@lucide/svelte/icons/ellipsis-vertical';
	import Search from '@lucide/svelte/icons/search';
	import Settings from '@lucide/svelte/icons/settings';
	import FolderInput from '@lucide/svelte/icons/folder-input';
	import CreateInstanceDialog from '$lib/components/instances/CreateInstanceDialog.svelte';
	import TemplatesFiltersSidebar from '$lib/components/TemplatesFiltersSidebar.svelte';
	import TemplateSettingsPanel from '$lib/components/templates/TemplateSettingsPanel.svelte';
	import AssignToProjectDialog from '$lib/components/templates/AssignToProjectDialog.svelte';
	import { Sheet, SheetContent, SheetTitle } from '$lib/components/ui/sheet';

	let templates = $state<TemplateSummary[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let dialogOpen = $state(false);
	let dialogTemplateId = $state<string | null>(null);
	let runCounts = $state<Record<string, { running: number; completed: number }>>({});
	let searchQuery = $state('');
	let projectFilter = $state<string | null>(null);
	let tagFilter = $state<string | null>(null);

	// Per-card Settings sheet + Assign-to-project dialog. Each holds the
	// target template so a single instance serves every card.
	let settingsOpen = $state(false);
	let settingsTemplate = $state<TemplateSummary | null>(null);
	let assignOpen = $state(false);
	let assignTemplate = $state<TemplateSummary | null>(null);

	// Opening a bits-ui overlay from inside a closing dropdown races the
	// dropdown's focus-return; defer to the next tick so the sheet/dialog
	// keeps focus instead of being dismissed immediately.
	function openSettings(t: TemplateSummary) {
		settingsTemplate = t;
		setTimeout(() => (settingsOpen = true), 0);
	}
	function openAssign(t: TemplateSummary) {
		assignTemplate = t;
		setTimeout(() => (assignOpen = true), 0);
	}

	function applyFilters(next: { projectId: string | null; tag: string | null }) {
		projectFilter = next.projectId;
		tagFilter = next.tag;
		load();
	}

	const filteredTemplates = $derived.by(() => {
		const q = searchQuery.trim().toLowerCase();
		if (!q) return templates;
		return templates.filter(
			(t) =>
				t.name.toLowerCase().includes(q) ||
				(t.description ?? '').toLowerCase().includes(q)
		);
	});

	// Per-template run tallies. Uses the paginated `total` from a perPage:1
	// query so we never pull the full instance list just to count.
	async function loadRunCounts(items: TemplateSummary[]) {
		const entries = await Promise.all(
			items.map(async (t) => {
				try {
					const [running, completed] = await Promise.all([
						listInstances({ templateId: t.id, status: 'running', perPage: 1 }),
						listInstances({ templateId: t.id, status: 'completed', perPage: 1 })
					]);
					return [t.id, { running: running.total, completed: completed.total }] as const;
				} catch {
					return [t.id, { running: 0, completed: 0 }] as const;
				}
			})
		);
		runCounts = Object.fromEntries(entries);
	}

	async function load() {
		loading = true;
		error = null;
		try {
			const result = await listTemplates(
				1,
				20,
				undefined,
				undefined,
				projectFilter ?? undefined,
				tagFilter ?? undefined
			);
			templates = result.items;
			loadRunCounts(result.items);
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

	// Surface the workflow's typed boundary on the list: Start `initial` field
	// names (inputs) and the union of End `resultMapping` target keys (outputs).
	// Multi-End templates collapse to the union — the editor card is where you
	// see per-End shape. `graph` ships with the list payload (typed `unknown`
	// in the schema), so no second fetch is needed.
	function summarize(graphRaw: unknown): { inputs: string[]; outputs: string[] } {
		const graph = graphRaw as WorkflowGraph | undefined;
		const nodes = graph?.nodes ?? [];
		const inputs: string[] = [];
		const outputs: string[] = [];
		const seenOutput = new Set<string>();
		for (const n of nodes) {
			if (n.data.type === 'start') {
				for (const f of n.data.initial?.fields ?? []) if (f.name) inputs.push(f.name);
			} else if (n.data.type === 'end') {
				for (const m of n.data.resultMapping ?? []) {
					if (m.targetField && !seenOutput.has(m.targetField)) {
						seenOutput.add(m.targetField);
						outputs.push(m.targetField);
					}
				}
			}
		}
		return { inputs, outputs };
	}
	const ioByTemplate = $derived(
		new Map(templates.map((t) => [t.id, summarize(t.graph)]))
	);

	// Load on mount
	$effect(() => {
		load();
	});
</script>

<div class="flex h-full" data-testid="templates-page">
	<TemplatesFiltersSidebar
		projectId={projectFilter}
		tag={tagFilter}
		onChange={applyFilters}
	/>
	<div class="flex-1 overflow-y-auto">
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

		<div class="relative mb-4">
			<Search class="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
			<Input
				type="search"
				placeholder="Search templates"
				bind:value={searchQuery}
				data-testid="input-search-templates"
				class="pl-9"
			/>
		</div>

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
		{:else if filteredTemplates.length === 0}
			<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
				<Search class="size-10 text-muted-foreground/40" />
				<p class="mt-3 text-sm text-muted-foreground">
					No templates match “{searchQuery}”
				</p>
			</div>
		{:else}
			<div class="space-y-2" data-testid="template-list">
				{#each filteredTemplates as template (template.id)}
					<a
						href="/templates/{template.id}"
						class="group flex flex-col gap-3 rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent/50"
						data-testid="template-item-{template.id}"
					>
						<div class="flex items-center justify-between gap-3 border-b border-border/60 pb-3">
							<div class="flex min-w-0 items-center gap-2">
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
									<span class="truncate text-sm font-medium text-foreground">{template.name}</span>
								{/if}
								<Badge class={template.published ? 'bg-green-100 text-green-700' : 'bg-amber-100 text-amber-700'} variant="secondary">
									{template.published ? 'Published' : 'Draft'} v{template.version}
								</Badge>
							</div>
							<div class="flex shrink-0 items-center gap-1">
								{#if template.published}
									<Button
										size="sm"
										data-testid="btn-run-template-{template.id}"
										onclick={(e: MouseEvent) => {
											e.preventDefault();
											e.stopPropagation();
											handleCreateInstance(template.id);
										}}
									>
										<Rocket class="size-4" />
										Run
									</Button>
								{/if}
								<DropdownMenu>
									<DropdownMenuTrigger
										data-testid="btn-template-menu-{template.id}"
										aria-label="Template actions"
										class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground data-[state=open]:bg-accent data-[state=open]:text-foreground"
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
											data-testid="btn-settings-template-{template.id}"
											onSelect={() => openSettings(template)}
										>
											<Settings class="size-4" />
											Settings
										</DropdownMenuItem>
										<DropdownMenuItem
											data-testid="btn-assign-project-template-{template.id}"
											onSelect={() => openAssign(template)}
										>
											<FolderInput class="size-4" />
											Assign to project
										</DropdownMenuItem>
										<DropdownMenuSeparator />
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
						</div>
						{#if template.description}
							<p class="truncate text-sm text-muted-foreground">{template.description}</p>
						{/if}
						{#if (ioByTemplate.get(template.id)?.inputs.length ?? 0) > 0 || (ioByTemplate.get(template.id)?.outputs.length ?? 0) > 0}
							{@const io = ioByTemplate.get(template.id) ?? { inputs: [], outputs: [] }}
							<div
								class="grid grid-cols-2 gap-x-6 gap-y-1 text-sm"
								data-testid="template-io-{template.id}"
							>
								<div class="flex flex-col gap-1">
									<span class="text-sm font-medium uppercase tracking-wider text-muted-foreground/70">
										In
									</span>
									<div class="flex flex-wrap gap-1.5">
										{#each io.inputs as name (name)}
											<span class="rounded border border-border bg-muted px-1.5 py-0.5 font-mono text-sm text-foreground">
												{name}
											</span>
										{:else}
											<span class="text-sm text-muted-foreground/60">—</span>
										{/each}
									</div>
								</div>
								<div class="flex flex-col gap-1">
									<span class="text-sm font-medium uppercase tracking-wider text-muted-foreground/70">
										Out
									</span>
									<div class="flex flex-wrap gap-1.5">
										{#each io.outputs as name (name)}
											<span class="rounded border border-border bg-muted px-1.5 py-0.5 font-mono text-sm text-foreground">
												{name}
											</span>
										{:else}
											<span class="text-sm text-muted-foreground/60">—</span>
										{/each}
									</div>
								</div>
							</div>
						{/if}
						{#if runCounts[template.id]}
							{@const c = runCounts[template.id]}
							<button
								type="button"
								class="inline-flex w-fit items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
								data-testid="btn-template-runs-{template.id}"
								onclick={(e: MouseEvent) => {
									e.preventDefault();
									e.stopPropagation();
									goto(`/instances?template_id=${template.id}`);
								}}
							>
								<Activity class="size-3" />
								{#if c.running > 0}
									<span class="text-blue-600">{c.running} running</span>
								{/if}
								{#if c.running > 0 && c.completed > 0}<span>&middot;</span>{/if}
								{#if c.completed > 0}
									<span>{c.completed} completed</span>
								{/if}
								{#if c.running === 0 && c.completed === 0}
									<span>No runs yet</span>
								{/if}
								<span class="underline">View runs</span>
							</button>
						{/if}
					</a>
				{/each}
			</div>
		{/if}
		</div>
	</div>
</div>

<CreateInstanceDialog
	bind:open={dialogOpen}
	templateId={dialogTemplateId}
	oncreated={onInstanceCreated}
/>

<Sheet.Root open={settingsOpen} onOpenChange={(o: boolean) => (settingsOpen = o)}>
	<SheetContent class="w-full max-w-md p-0 sm:max-w-md">
		<SheetTitle class="sr-only">Template settings</SheetTitle>
		{#if settingsTemplate}
			<TemplateSettingsPanel template={settingsTemplate} />
		{/if}
	</SheetContent>
</Sheet.Root>

<AssignToProjectDialog
	bind:open={assignOpen}
	templateId={assignTemplate?.id ?? null}
	templateName={assignTemplate?.name}
/>
