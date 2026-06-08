<script lang="ts">
	import {
		listFileServers,
		createFileServer,
		adoptFileServer,
		updateFileServer,
		deleteFileServer,
		type FileServerView,
		type UnregisteredServer
	} from '$lib/api/fileServers';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Select from '$lib/components/ui/select';
	import * as Dialog from '$lib/components/ui/dialog';
	import Server from '@lucide/svelte/icons/server';
	import Archive from '@lucide/svelte/icons/archive';
	import FolderTree from '@lucide/svelte/icons/folder-tree';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Link2Off from '@lucide/svelte/icons/link-2-off';

	let servers = $state<FileServerView[]>([]);
	let unregistered = $state<UnregisteredServer[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Form state (shared by create / adopt / edit).
	let formOpen = $state(false);
	let formMode = $state<'create' | 'adopt' | 'edit'>('create');
	let fKey = $state('');
	let fDisplayName = $state('');
	let fKind = $state('s3');
	let fResourceRef = $state('');
	let fBasePath = $state('');
	let formError = $state<string | null>(null);
	let saving = $state(false);
	let resources = $state<ResourceSummary[]>([]);

	// kinds that reference a workspace resource for secrets. object_store uses
	// platform config (no resource); s3/sftp pick a resource of the matching type.
	const kinds = ['object_store', 's3', 'sftp'];
	const kindNeedsResource = (k: string) => k === 's3' || k === 'sftp';

	function kindIcon(k: string) {
		return k === 'object_store' ? Archive : k === 'sftp' ? FolderTree : Server;
	}

	function fmtSize(n: number): string {
		if (n < 1024) return `${n} B`;
		const u = ['KB', 'MB', 'GB', 'TB'];
		let v = n / 1024, i = 0;
		while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
		return `${v.toFixed(1)} ${u[i]}`;
	}

	async function load() {
		loading = true;
		error = null;
		try {
			const r = await listFileServers();
			servers = r.servers;
			unregistered = r.unregistered;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load servers';
		} finally {
			loading = false;
		}
	}

	async function loadResourcesForKind(kind: string) {
		if (!kindNeedsResource(kind)) { resources = []; return; }
		try {
			const r = await listResources({ resource_type: kind, perPage: 100 });
			resources = r.items ?? [];
		} catch {
			resources = [];
		}
	}

	function openCreate() {
		formMode = 'create';
		fKey = ''; fDisplayName = ''; fKind = 's3'; fResourceRef = ''; fBasePath = '';
		formError = null;
		loadResourcesForKind(fKind);
		formOpen = true;
	}
	function openAdopt(u: UnregisteredServer) {
		formMode = 'adopt';
		fKey = u.key; fDisplayName = u.key; fKind = 'sftp'; fResourceRef = ''; fBasePath = '';
		formError = null;
		loadResourcesForKind(fKind);
		formOpen = true;
	}
	function openEdit(s: FileServerView) {
		formMode = 'edit';
		fKey = s.key; fDisplayName = s.display_name; fKind = s.kind;
		fResourceRef = s.resource_ref ?? ''; fBasePath = s.base_path ?? '';
		formError = null;
		loadResourcesForKind(s.kind);
		formOpen = true;
	}

	async function save() {
		saving = true;
		formError = null;
		try {
			const body = {
				key: fKey,
				display_name: fDisplayName || undefined,
				kind: fKind,
				resource_ref: kindNeedsResource(fKind) ? fResourceRef || undefined : undefined,
				base_path: fBasePath || undefined
			};
			if (formMode === 'edit') {
				await updateFileServer(fKey, {
					display_name: fDisplayName || undefined,
					kind: fKind,
					resource_ref: kindNeedsResource(fKind) ? fResourceRef || null : null,
					base_path: fBasePath || null
				});
			} else if (formMode === 'adopt') {
				await adoptFileServer(body);
			} else {
				await createFileServer(body);
			}
			formOpen = false;
			await load();
		} catch (e) {
			formError = e instanceof Error ? e.message : 'Save failed';
		} finally {
			saving = false;
		}
	}

	async function remove(s: FileServerView) {
		if (!confirm(`Delete file server "${s.display_name}"? Inventory rows are kept (the server becomes unregistered).`)) return;
		try {
			await deleteFileServer(s.key);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Delete failed';
		}
	}

	$effect(() => { load(); });
</script>

<div class="mb-4 flex items-center justify-between">
	<p class="text-sm text-muted-foreground">Storage backends the platform tracks files on. Secrets live in the linked resource.</p>
	<Button size="sm" onclick={openCreate} data-testid="server-create"><Plus class="size-4" /> Add server</Button>
</div>

{#if error}
	<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-200">{error}</div>
{/if}

{#if loading}
	<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">Loading…</div>
{:else}
	<!-- Registered servers -->
	{#if servers.length === 0}
		<div class="rounded-xl border border-dashed border-border py-12 text-center text-sm text-muted-foreground">No file servers registered yet.</div>
	{:else}
		<div class="space-y-1.5">
			{#each servers as s (s.key)}
				{@const Icon = kindIcon(s.kind)}
				<div class="flex items-center gap-3 rounded-lg border border-border bg-card px-4 py-3" data-testid="server-row">
					<Icon class="size-5 shrink-0 text-muted-foreground" />
					<div class="min-w-0 flex-1">
						<div class="flex items-center gap-2">
							<span class="truncate font-medium text-foreground">{s.display_name}</span>
							<Badge variant="outline" class="px-1.5 py-0 text-[10px]">{s.kind}</Badge>
							{#if s.status === 'online'}<span class="size-2 rounded-full bg-green-500" title="online"></span>{/if}
						</div>
						<div class="mt-0.5 flex items-center gap-3 text-sm text-muted-foreground">
							<span class="font-mono">{s.key}</span>
							{#if s.resource_ref}
								<span class="inline-flex items-center gap-1" class:text-amber-600={!s.resource_resolves} title={s.resource_resolves ? 'resource' : 'resource not found'}>
									{#if !s.resource_resolves}<Link2Off class="size-3" />{/if}
									→ {s.resource_ref}
								</span>
							{/if}
							{#if s.base_path}<span class="font-mono">/{s.base_path.replace(/^\/+/, '')}</span>{/if}
						</div>
					</div>
					<div class="shrink-0 text-right">
						<div class="text-sm font-semibold tabular-nums text-foreground">{s.file_count.toLocaleString()} files</div>
						<div class="text-sm tabular-nums text-muted-foreground">{fmtSize(s.total_size_bytes)}</div>
					</div>
					<div class="flex shrink-0 items-center gap-1">
						<Button variant="ghost" size="icon-sm" onclick={() => openEdit(s)} title="Edit"><Pencil class="size-4" /></Button>
						{#if s.kind !== 'object_store'}
							<Button variant="ghost" size="icon-sm" onclick={() => remove(s)} title="Delete"><Trash2 class="size-4" /></Button>
						{/if}
					</div>
				</div>
			{/each}
		</div>
	{/if}

	<!-- Unregistered inventory keys (adopt candidates) -->
	{#if unregistered.length > 0}
		<div class="mt-6">
			<h3 class="mb-2 text-sm font-semibold uppercase tracking-wider text-muted-foreground">Unregistered ({unregistered.length})</h3>
			<p class="mb-2 text-sm text-muted-foreground">Server names seen in inventory with no entity yet — adopt to manage and crawl them.</p>
			<div class="space-y-1.5">
				{#each unregistered as u (u.key)}
					<div class="flex items-center gap-3 rounded-lg border border-dashed border-border px-4 py-2.5" data-testid="server-unregistered">
						<Server class="size-4 shrink-0 text-muted-foreground/60" />
						<span class="min-w-0 flex-1 truncate font-mono text-sm text-foreground">{u.key}</span>
						<span class="shrink-0 text-sm tabular-nums text-muted-foreground">{u.file_count.toLocaleString()} files · {fmtSize(u.total_size_bytes)}</span>
						<Button variant="outline" size="sm" onclick={() => openAdopt(u)}>Adopt</Button>
					</div>
				{/each}
			</div>
		</div>
	{/if}
{/if}

<!-- Create / adopt / edit dialog -->
<Dialog.Root bind:open={formOpen}>
	<Dialog.Content class="sm:max-w-md">
		<Dialog.Header>
			<Dialog.Title>{formMode === 'edit' ? 'Edit file server' : formMode === 'adopt' ? 'Adopt file server' : 'Add file server'}</Dialog.Title>
		</Dialog.Header>
		<div class="space-y-3">
			<div>
				<!-- svelte-ignore a11y_label_has_associated_control -->
				<label class="mb-1 block text-sm font-medium text-foreground">Key</label>
				<Input bind:value={fKey} disabled={formMode !== 'create'} placeholder="lab-nas-1" class="text-sm" />
				<p class="mt-1 text-xs text-muted-foreground">Stable id; matches the inventory <code>file_server_id</code>.</p>
			</div>
			<div>
				<!-- svelte-ignore a11y_label_has_associated_control -->
				<label class="mb-1 block text-sm font-medium text-foreground">Display name</label>
				<Input bind:value={fDisplayName} placeholder="Lab NAS" class="text-sm" />
			</div>
			<div>
				<!-- svelte-ignore a11y_label_has_associated_control -->
				<label class="mb-1 block text-sm font-medium text-foreground">Kind</label>
				<Select.Root type="single" value={fKind} onValueChange={(v) => { if (v) { fKind = v; fResourceRef = ''; loadResourcesForKind(v); } }}>
					<Select.Trigger class="text-sm">{fKind}</Select.Trigger>
					<Select.Content>
						{#each kinds as k}<Select.Item value={k} label={k} />{/each}
					</Select.Content>
				</Select.Root>
			</div>
			{#if kindNeedsResource(fKind)}
				<div>
					<!-- svelte-ignore a11y_label_has_associated_control -->
					<label class="mb-1 block text-sm font-medium text-foreground">Resource ({fKind} credentials)</label>
					{#if resources.length > 0}
						<Select.Root type="single" value={fResourceRef} onValueChange={(v) => (fResourceRef = v ?? '')}>
							<Select.Trigger class="text-sm">{fResourceRef || 'Select a resource…'}</Select.Trigger>
							<Select.Content>
								{#each resources as r}<Select.Item value={r.path} label={`${r.display_name} (${r.path})`} />{/each}
							</Select.Content>
						</Select.Root>
					{:else}
						<Input bind:value={fResourceRef} placeholder="resource path (e.g. lab_nas)" class="text-sm" />
						<p class="mt-1 text-xs text-muted-foreground">No <code>{fKind}</code> resources found — create one under Resources, or type its path.</p>
					{/if}
				</div>
			{/if}
			<div>
				<!-- svelte-ignore a11y_label_has_associated_control -->
				<label class="mb-1 block text-sm font-medium text-foreground">Base path <span class="font-normal text-muted-foreground">(optional)</span></label>
				<Input bind:value={fBasePath} placeholder="legacy/datasets" class="text-sm" />
			</div>
			{#if formError}<p class="text-sm text-red-600">{formError}</p>{/if}
		</div>
		<Dialog.Footer>
			<Button variant="ghost" onclick={() => (formOpen = false)}>Cancel</Button>
			<Button onclick={save} disabled={saving || !fKey}>{saving ? 'Saving…' : 'Save'}</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
