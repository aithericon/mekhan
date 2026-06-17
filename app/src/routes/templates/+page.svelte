<script lang="ts">
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import {
		listTemplates,
		createTemplate,
		deleteTemplate,
		updateTemplate,
		createNewVersion,
		createInstance,
		forkTemplate,
		listInstances,
		type TemplateSummary
	} from '$lib/api/client';
	import { toast } from 'svelte-sonner';
	import { workspaces } from '$lib/workspaces/store.svelte';
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
	import GitFork from '@lucide/svelte/icons/git-fork';
	import Activity from '@lucide/svelte/icons/activity';
	import EllipsisVertical from '@lucide/svelte/icons/ellipsis-vertical';
	import Search from '@lucide/svelte/icons/search';
	import ArrowUpDown from '@lucide/svelte/icons/arrow-up-down';
	import Settings from '@lucide/svelte/icons/settings';
	import FolderInput from '@lucide/svelte/icons/folder-input';
	import CreateInstanceDialog from '$lib/components/instances/CreateInstanceDialog.svelte';
	import ForkToWorkspaceDialog from '$lib/components/ForkToWorkspaceDialog.svelte';
	import { PageShell, PageHeader } from '$lib/components/shell';
	import TemplatesFiltersSidebar from '$lib/components/TemplatesFiltersSidebar.svelte';
	import TemplateSettingsPanel from '$lib/components/templates/TemplateSettingsPanel.svelte';
	import MoveToFolderDialog from '$lib/components/templates/MoveToFolderDialog.svelte';
	import { Sheet, SheetContent, SheetTitle } from '$lib/components/ui/sheet';
	import ShareDialog from '$lib/components/iam/ShareDialog.svelte';
	import AuthorshipChips from '$lib/components/iam/AuthorshipChips.svelte';
	import Share2 from '@lucide/svelte/icons/share-2';
	import { roleAtLeast } from '$lib/api/iam';

	let templates = $state<TemplateSummary[]>([]);
	let loading = $state(true);
	let loadingMore = $state(false);
	let error = $state<string | null>(null);
	let dialogOpen = $state(false);
	let dialogTemplateId = $state<string | null>(null);
	let runCounts = $state<Record<string, { running: number; completed: number }>>({});
	let searchQuery = $state('');
	let folderFilter = $state<string | null>(null);
	let recursiveFilter = $state(false);
	let tagFilter = $state<string | null>(null);

	// Server-driven pagination (0-based) + sort. Search/sort/filter all run on
	// the backend now; results accumulate via "Load more".
	let sort = $state('-updated_at');
	let page = $state(0);
	let total = $state(0);
	let hasNext = $state(false);
	const PAGE_SIZE = 20;

	const SORT_OPTIONS = [
		{ value: '-updated_at', label: 'Recently updated' },
		{ value: '-created_at', label: 'Newly created' },
		{ value: 'name', label: 'Name (A–Z)' },
		{ value: '-name', label: 'Name (Z–A)' },
		{ value: '-version', label: 'Version' }
	];
	const sortLabel = $derived(SORT_OPTIONS.find((o) => o.value === sort)?.label ?? 'Sort');
	const hasQuery = $derived(!!searchQuery.trim() || !!folderFilter || !!tagFilter);

	// Per-card Settings sheet + Move-to-folder dialog. Each holds the
	// target template so a single instance serves every card.
	let settingsOpen = $state(false);
	let settingsTemplate = $state<TemplateSummary | null>(null);
	let moveOpen = $state(false);
	let moveTemplate = $state<TemplateSummary | null>(null);

	// Per-object share (Phase 5). Editing/deleting needs editor on the template;
	// Share needs object-Admin. `my_effective_role` rides the list row (Phase 3
	// batch resolver) so the per-row gate is N+0 calls.
	let shareOpen = $state(false);
	let shareTemplate = $state<TemplateSummary | null>(null);
	function openShare(t: TemplateSummary) {
		shareTemplate = t;
		setTimeout(() => (shareOpen = true), 0);
	}

	// Opening a bits-ui overlay from inside a closing dropdown races the
	// dropdown's focus-return; defer to the next tick so the sheet/dialog
	// keeps focus instead of being dismissed immediately.
	function openSettings(t: TemplateSummary) {
		settingsTemplate = t;
		setTimeout(() => (settingsOpen = true), 0);
	}
	function openMove(t: TemplateSummary) {
		moveTemplate = t;
		setTimeout(() => (moveOpen = true), 0);
	}

	// Sidebar folder/tag selection reloads from page 0.
	function applyFilters(next: { folderId: string | null; recursive: boolean; tag: string | null }) {
		folderFilter = next.folderId;
		recursiveFilter = next.recursive;
		tagFilter = next.tag;
		loadFirst();
	}

	// Debounce search keystrokes so we don't refetch on every character.
	let searchTimer: ReturnType<typeof setTimeout> | undefined;
	function onSearchInput() {
		clearTimeout(searchTimer);
		searchTimer = setTimeout(loadFirst, 250);
	}

	function setSort(value: string) {
		if (value === sort) return;
		sort = value;
		loadFirst();
	}

	// Per-template run tallies. Uses the paginated `total` from a perPage:1
	// query so we never pull the full instance list just to count. Results are
	// merged so already-loaded pages keep their counts when more are appended.
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
		runCounts = { ...runCounts, ...Object.fromEntries(entries) };
	}

	function fetchPage(p: number) {
		return listTemplates({
			page: p,
			pageSize: PAGE_SIZE,
			search: searchQuery.trim() || undefined,
			sort,
			folderId: folderFilter ?? undefined,
			recursive: folderFilter ? recursiveFilter : undefined,
			tag: tagFilter ?? undefined
		});
	}

	// (Re)load from page 0, replacing the list. Driven by search/sort/filter.
	async function loadFirst() {
		loading = true;
		error = null;
		try {
			const result = await fetchPage(0);
			templates = result.items ?? [];
			total = result.total ?? templates.length;
			page = result.page ?? 0;
			hasNext = result.has_next ?? false;
			loadRunCounts(templates);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load templates';
			templates = [];
			total = 0;
			hasNext = false;
		} finally {
			loading = false;
		}
	}

	// Append the next page to the current list.
	async function loadMore() {
		if (loadingMore || !hasNext) return;
		loadingMore = true;
		error = null;
		try {
			const next = page + 1;
			const result = await fetchPage(next);
			templates = [...templates, ...(result.items ?? [])];
			page = result.page ?? next;
			hasNext = result.has_next ?? false;
			loadRunCounts(result.items ?? []);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load more';
		} finally {
			loadingMore = false;
		}
	}

	const remaining = $derived(Math.max(0, total - templates.length));

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

	// Deep-copy a (typically public / cross-workspace) template — e.g. a built-in
	// demo — into a workspace the caller owns, where it becomes runnable as their
	// own. When the caller can write to more than one workspace we ask which;
	// otherwise we fork straight in.
	let forkingId = $state<string | null>(null);
	let forkDialogOpen = $state(false);
	let forkTarget = $state<TemplateSummary | null>(null);

	const writableWorkspaces = $derived(
		workspaces.workspaces.filter((w) => !w.is_system && roleAtLeast(w.my_role, 'editor'))
	);
	// Default the picker to the active workspace when it's writable, else the first.
	const defaultForkWorkspaceId = $derived(
		writableWorkspaces.find((w) => w.id === workspaces.active?.id)?.id ??
			writableWorkspaces[0]?.id
	);

	function requestFork(template: TemplateSummary) {
		if (forkingId) return;
		if (writableWorkspaces.length > 1) {
			forkTarget = template;
			forkDialogOpen = true;
		} else {
			void doFork(template, writableWorkspaces[0]?.id);
		}
	}

	async function doFork(template: TemplateSummary, targetWorkspaceId?: string) {
		if (forkingId) return;
		forkingId = template.id;
		forkDialogOpen = false;
		try {
			const forked = await forkTemplate(template.id, targetWorkspaceId);
			const dest =
				workspaces.workspaces.find((w) => w.id === forked.workspace_id)?.display_name ??
				'your workspace';
			toast.success(`Forked "${template.name}" into ${dest}`);
			// Only open the new copy when it lives in the workspace you're in;
			// forking from demos lands it elsewhere, so stay on the browse view.
			if (forked.workspace_id === workspaces.active?.id) {
				await goto(`/templates/${forked.id}`);
			} else {
				forkingId = null;
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'Failed to fork template');
			forkingId = null;
		}
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
	// The list payload no longer ships the full `graph` (it was ~20 MB / page);
	// the server now computes this compact preview straight from the graph and
	// returns it as `io_inputs` / `io_outputs` on each summary.
	const ioByTemplate = $derived(
		new Map(
			templates.map((t) => [t.id, { inputs: t.io_inputs ?? [], outputs: t.io_outputs ?? [] }])
		)
	);

	onMount(loadFirst);
</script>

<PageShell testid="templates-page">
	{#snippet band()}
		<PageHeader title="Templates" subtitle="Create and manage workflow templates">
			{#snippet actions()}
				<Button data-testid="btn-create-template" onclick={handleCreate}>
					<Plus class="size-4" />
					New Template
				</Button>
			{/snippet}
		</PageHeader>
	{/snippet}

	{#snippet sidebar()}
		<TemplatesFiltersSidebar
			folderId={folderFilter}
			recursive={recursiveFilter}
			tag={tagFilter}
			onChange={applyFilters}
		/>
	{/snippet}

		{#if error}
			<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
				{error}
			</div>
		{/if}

		<div class="mb-4 flex items-center gap-2">
			<div class="relative flex-1">
				<Search class="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
				<Input
					type="search"
					placeholder="Search templates"
					bind:value={searchQuery}
					oninput={onSearchInput}
					data-testid="input-search-templates"
					class="pl-9"
				/>
			</div>
			<DropdownMenu>
				<DropdownMenuTrigger
					data-testid="btn-sort-templates"
					aria-label="Sort templates"
					class="inline-flex h-9 shrink-0 items-center gap-2 rounded-md border border-border bg-card px-3 text-sm text-foreground transition-colors hover:bg-accent"
				>
					<ArrowUpDown class="size-4 text-muted-foreground" />
					{sortLabel}
				</DropdownMenuTrigger>
				<DropdownMenuContent align="end">
					{#each SORT_OPTIONS as opt (opt.value)}
						<DropdownMenuItem
							data-testid="sort-option-{opt.value}"
							onSelect={() => setSort(opt.value)}
						>
							{opt.label}
						</DropdownMenuItem>
					{/each}
				</DropdownMenuContent>
			</DropdownMenu>
		</div>

		{#if loading}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Loading...
			</div>
		{:else if templates.length === 0 && hasQuery}
			<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
				<Search class="size-10 text-muted-foreground/40" />
				<p class="mt-3 text-sm text-muted-foreground">No templates match your filters</p>
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
					{@const canEdit = roleAtLeast(template.my_effective_role, 'editor')}
					{@const canShareT = roleAtLeast(template.my_effective_role, 'admin')}
					{@const readOnly = !canEdit}
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
								{#if template.published && canEdit}
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
								{:else if readOnly}
									<!-- Read-only here (e.g. browsing the demos workspace): you can't
									     run it in place; fork it into a workspace you own, then run. -->
									<Button
										size="sm"
										variant="outline"
										data-testid="btn-fork-template-{template.id}"
										disabled={forkingId === template.id}
										onclick={(e: MouseEvent) => {
											e.preventDefault();
											e.stopPropagation();
											requestFork(template);
										}}
									>
										<GitFork class="size-4" />
										{forkingId === template.id ? 'Forking…' : 'Fork to workspace'}
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
										{#if canEdit && !template.published}
											<DropdownMenuItem
												data-testid="btn-rename-template-{template.id}"
												onSelect={() => startRename(template)}
											>
												<Pencil class="size-4" />
												Rename
											</DropdownMenuItem>
											<DropdownMenuSeparator />
										{:else if canEdit}
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
										{#if canShareT}
											<DropdownMenuItem
												data-testid="btn-share-template-{template.id}"
												onSelect={() => openShare(template)}
											>
												<Share2 class="size-4" />
												Share
											</DropdownMenuItem>
										{/if}
										<DropdownMenuItem
											data-testid="btn-settings-template-{template.id}"
											onSelect={() => openSettings(template)}
										>
											<Settings class="size-4" />
											Settings
										</DropdownMenuItem>
										{#if canEdit}
											<DropdownMenuItem
												data-testid="btn-move-folder-template-{template.id}"
												onSelect={() => openMove(template)}
											>
												<FolderInput class="size-4" />
												Move to folder
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
										{/if}
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
						<AuthorshipChips
							createdBy={template.author_id}
							createdAt={template.created_at}
							updatedBy={template.updated_by}
							updatedAt={template.updated_at}
						/>
					</a>
				{/each}
			</div>
			{#if hasNext}
				<div class="mt-4 flex flex-col items-center gap-2">
					<Button
						variant="outline"
						onclick={loadMore}
						disabled={loadingMore}
						data-testid="btn-load-more-templates"
					>
						{loadingMore ? 'Loading…' : `Load ${Math.min(PAGE_SIZE, remaining)} more`}
					</Button>
					<p class="text-sm text-muted-foreground">Showing {templates.length} of {total}</p>
				</div>
			{:else if total > 0}
				<p class="mt-4 text-center text-sm text-muted-foreground">Showing all {total}</p>
			{/if}
		{/if}
	</PageShell>

<CreateInstanceDialog
	bind:open={dialogOpen}
	templateId={dialogTemplateId}
	oncreated={onInstanceCreated}
/>

{#if forkTarget}
	<ForkToWorkspaceDialog
		bind:open={forkDialogOpen}
		itemName={forkTarget.name}
		options={writableWorkspaces}
		defaultId={defaultForkWorkspaceId}
		submitting={forkingId === forkTarget.id}
		onConfirm={(wsId) => forkTarget && doFork(forkTarget, wsId)}
	/>
{/if}

<Sheet.Root open={settingsOpen} onOpenChange={(o: boolean) => (settingsOpen = o)}>
	<SheetContent class="w-full max-w-md p-0 sm:max-w-md">
		<SheetTitle class="sr-only">Template settings</SheetTitle>
		{#if settingsTemplate}
			<TemplateSettingsPanel template={settingsTemplate} />
		{/if}
	</SheetContent>
</Sheet.Root>

<MoveToFolderDialog
	bind:open={moveOpen}
	templateId={moveTemplate?.id ?? null}
	templateName={moveTemplate?.name}
	onMoved={loadFirst}
/>

{#if shareTemplate}
	<ShareDialog
		bind:open={shareOpen}
		objectType="template"
		objectId={shareTemplate.id}
		objectName={shareTemplate.name}
		myEffectiveRole={shareTemplate.my_effective_role}
		onChanged={loadFirst}
	/>
{/if}
