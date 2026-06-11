<script lang="ts">
	import {
		listFileServers,
		createFileServer,
		adoptFileServer,
		updateFileServer,
		deleteFileServer,
		createEndpoint,
		updateEndpoint,
		deleteEndpoint,
		verifyEndpoint,
		type FileServerView,
		type FileServerEndpoint,
		type UnregisteredServer,
		type CreateEndpointRequest
	} from '$lib/api/fileServers';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import { formatBytes } from './format';
	import { listCapacities } from '$lib/api/capacities';
	import { listRunners } from '$lib/api/runners';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Select from '$lib/components/ui/select';
	import * as Dialog from '$lib/components/ui/dialog';
	import Server from '@lucide/svelte/icons/server';
	import Cloud from '@lucide/svelte/icons/cloud';
	import Database from '@lucide/svelte/icons/database';
	import HardDrive from '@lucide/svelte/icons/hard-drive';
	import FolderTree from '@lucide/svelte/icons/folder-tree';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Link2Off from '@lucide/svelte/icons/link-2-off';
	import ShieldCheck from '@lucide/svelte/icons/shield-check';
	import LoaderCircle from '@lucide/svelte/icons/loader-circle';
	import { toast } from 'svelte-sonner';

	// When set (a server link was clicked in Entries/Copies), highlight that row.
	let { focusKey }: { focusKey?: string | null } = $props();

	let servers = $state<FileServerView[]>([]);
	let unregistered = $state<UnregisteredServer[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// access methods (transports) an endpoint can carry.
	const accessMethods = ['object_store', 's3', 'sftp', 'local_mount'];
	// methods that reference a workspace resource for connection + secrets.
	const methodNeedsResource = (m: string) => m === 's3' || m === 'sftp';

	function methodIcon(m: string) {
		return m === 'object_store'
			? Cloud
			: m === 's3'
				? Database
				: m === 'sftp'
					? FolderTree
					: m === 'local_mount'
						? HardDrive
						: Server;
	}

	function verificationVariant(v: string): { class: string; label: string } {
		switch (v) {
			case 'verified':
				return {
					class:
						'border-green-300 bg-green-50 text-green-700 dark:border-green-900 dark:bg-green-950 dark:text-green-300',
					label: 'verified'
				};
			case 'mismatch':
				return {
					class:
						'border-red-300 bg-red-50 text-red-700 dark:border-red-900 dark:bg-red-950 dark:text-red-300',
					label: 'mismatch'
				};
			case 'conflict':
				return {
					class:
						'border-amber-300 bg-amber-50 text-amber-700 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-300',
					label: 'conflict'
				};
			default:
				return { class: 'text-muted-foreground', label: v || 'unverified' };
		}
	}

	function statusDot(status: string): string {
		return status === 'online'
			? 'bg-green-500'
			: status === 'offline'
				? 'bg-red-500'
				: 'bg-muted-foreground/40';
	}

	/** A server's resource_refs that fail to resolve (rollup is server-level only). */
	function unresolvedRef(s: FileServerView, ep: FileServerEndpoint): boolean {
		// `resource_resolves` is the whole-server aggregate; flag a non-null ref
		// only when the aggregate says something is broken.
		return !s.resource_resolves && !!ep.resource_ref;
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

	// ---- Resource picker (shared by server-create + endpoint dialogs) --------
	let resources = $state<ResourceSummary[]>([]);
	async function loadResourcesForMethod(method: string) {
		if (!methodNeedsResource(method)) {
			resources = [];
			return;
		}
		try {
			const r = await listResources({ resource_type: method, perPage: 100 });
			resources = r.items ?? [];
		} catch {
			resources = [];
		}
	}

	// ---- Capacity-group picker (local_mount dispatch target) ----------------
	// A `local_mount` endpoint dispatches serve requests to `fileserve.<group>`
	// where <group> is a capacity pool's resource UUID or a single runner's id —
	// the same partition its co-located daemon consumes jobs on. Offer those by
	// name instead of asking for a raw UUID.
	let groupOptions = $state<{ value: string; label: string }[]>([]);
	let groupOptionsLoaded = false;
	async function loadGroupOptions() {
		if (groupOptionsLoaded) return;
		groupOptionsLoaded = true;
		const [caps, runners] = await Promise.all([
			listCapacities().catch(() => []),
			listRunners({ perPage: 100 }).catch(() => null)
		]);
		groupOptions = [
			...caps.map((c) => ({ value: c.id, label: `${c.display_name} — pool` })),
			...(runners?.items ?? []).map((r) => ({ value: r.id, label: `${r.name} — runner` }))
		];
	}
	function groupLabelFor(v: string, emptyLabel: string): string {
		if (!v) return emptyLabel;
		return groupOptions.find((o) => o.value === v)?.label ?? `${v.slice(0, 8)}… (unlisted id)`;
	}

	// ---- Server create / adopt / edit dialog --------------------------------
	let serverFormOpen = $state(false);
	let serverFormMode = $state<'create' | 'adopt' | 'edit'>('create');
	let fKey = $state('');
	let fDisplayName = $state('');
	// First inline endpoint (create / adopt only).
	let fMethod = $state('s3');
	let fResourceRef = $state('');
	let fRoot = $state('');
	let fGroupId = $state('');
	let serverFormError = $state<string | null>(null);
	let savingServer = $state(false);

	function openCreate() {
		serverFormMode = 'create';
		fKey = '';
		fDisplayName = '';
		fMethod = 's3';
		fResourceRef = '';
		fRoot = '';
		fGroupId = '';
		serverFormError = null;
		loadResourcesForMethod(fMethod);
		loadGroupOptions();
		serverFormOpen = true;
	}
	function openAdopt(u: UnregisteredServer) {
		serverFormMode = 'adopt';
		fKey = u.key;
		fDisplayName = u.key;
		fMethod = 'local_mount';
		fResourceRef = '';
		fRoot = '';
		fGroupId = '';
		serverFormError = null;
		loadResourcesForMethod(fMethod);
		loadGroupOptions();
		serverFormOpen = true;
	}
	function openEditServer(s: FileServerView) {
		serverFormMode = 'edit';
		fKey = s.key;
		fDisplayName = s.display_name;
		serverFormError = null;
		serverFormOpen = true;
	}

	function inlineEndpoint(): CreateEndpointRequest {
		return {
			access_method: fMethod,
			root: fRoot || undefined,
			resource_ref: methodNeedsResource(fMethod) ? fResourceRef || undefined : undefined,
			group_id: fMethod === 'local_mount' ? fGroupId || undefined : undefined
		};
	}

	async function saveServer() {
		savingServer = true;
		serverFormError = null;
		try {
			if (serverFormMode === 'edit') {
				await updateFileServer(fKey, {
					display_name: fDisplayName || undefined
				});
			} else {
				const body = {
					key: fKey,
					display_name: fDisplayName || undefined,
					endpoint: inlineEndpoint()
				};
				if (serverFormMode === 'adopt') {
					await adoptFileServer(body);
				} else {
					await createFileServer(body);
				}
			}
			serverFormOpen = false;
			await load();
		} catch (e) {
			serverFormError = e instanceof Error ? e.message : 'Save failed';
		} finally {
			savingServer = false;
		}
	}

	async function removeServer(s: FileServerView) {
		if (
			!confirm(
				`Delete file server "${s.display_name}"? Its endpoints are removed; inventory rows are kept (the server becomes unregistered).`
			)
		)
			return;
		try {
			await deleteFileServer(s.key);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Delete failed';
		}
	}

	// ---- Endpoint add / edit dialog -----------------------------------------
	let epFormOpen = $state(false);
	let epFormMode = $state<'create' | 'edit'>('create');
	let epServerKey = $state('');
	let epId = $state('');
	let epMethod = $state('s3');
	let epRoot = $state('');
	let epResourceRef = $state('');
	let epGroupId = $state('');
	let epPriority = $state('0');
	let epFormError = $state<string | null>(null);
	let savingEp = $state(false);

	function openAddEndpoint(s: FileServerView) {
		epFormMode = 'create';
		epServerKey = s.key;
		epId = '';
		epMethod = 's3';
		epRoot = '';
		epResourceRef = '';
		epGroupId = '';
		epPriority = '0';
		epFormError = null;
		loadResourcesForMethod(epMethod);
		loadGroupOptions();
		epFormOpen = true;
	}
	function openEditEndpoint(s: FileServerView, ep: FileServerEndpoint) {
		epFormMode = 'edit';
		epServerKey = s.key;
		epId = ep.id;
		epMethod = ep.access_method;
		epRoot = ep.root ?? '';
		epResourceRef = ep.resource_ref ?? '';
		epGroupId = ep.group_id ?? '';
		epPriority = String(ep.priority ?? 0);
		epFormError = null;
		loadResourcesForMethod(ep.access_method);
		loadGroupOptions();
		epFormOpen = true;
	}

	async function saveEndpoint() {
		savingEp = true;
		epFormError = null;
		const priority = Number.parseInt(epPriority, 10);
		try {
			if (epFormMode === 'edit') {
				await updateEndpoint(epServerKey, epId, {
					access_method: epMethod,
					root: epRoot || null,
					resource_ref: methodNeedsResource(epMethod) ? epResourceRef || null : null,
					group_id: epMethod === 'local_mount' ? epGroupId || null : null,
					priority: Number.isFinite(priority) ? priority : 0
				});
			} else {
				await createEndpoint(epServerKey, {
					access_method: epMethod,
					root: epRoot || undefined,
					resource_ref: methodNeedsResource(epMethod) ? epResourceRef || undefined : undefined,
					group_id: epMethod === 'local_mount' ? epGroupId || undefined : undefined,
					priority: Number.isFinite(priority) ? priority : undefined
				});
			}
			epFormOpen = false;
			await load();
		} catch (e) {
			epFormError = e instanceof Error ? e.message : 'Save failed';
		} finally {
			savingEp = false;
		}
	}

	// Endpoint ids with an in-flight hash-probe (verify is sampled reads through
	// the live transport — runner/s3/sftp — so it can take a few seconds).
	let verifying = $state<Set<string>>(new Set());

	async function runVerify(s: FileServerView, ep: FileServerEndpoint) {
		verifying = new Set([...verifying, ep.id]);
		try {
			const r = await verifyEndpoint(s.key, ep.id);
			const counts = `${r.passed}/${r.sampled} passed${r.missing ? `, ${r.missing} missing` : ''}`;
			if (r.verification_status === 'verified') {
				toast.success(`Endpoint verified — ${counts}`);
			} else {
				const example = r.examples?.[0];
				toast.error(
					`Endpoint ${r.verification_status} — ${r.mismatched} mismatched of ${r.sampled}` +
						(example ? ` (e.g. ${example.path})` : '')
				);
			}
			await load();
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'verification failed');
		} finally {
			const next = new Set(verifying);
			next.delete(ep.id);
			verifying = next;
		}
	}

	async function removeEndpoint(s: FileServerView, ep: FileServerEndpoint) {
		if (!confirm(`Remove the ${ep.access_method} endpoint from "${s.display_name}"?`)) return;
		try {
			await deleteEndpoint(s.key, ep.id);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Delete failed';
		}
	}

	$effect(() => {
		load();
	});
</script>

<div class="mb-4 flex items-center justify-between">
	<p class="text-sm text-muted-foreground">
		Storage backends the platform tracks files on. Each server has one or more access endpoints;
		secrets live in the linked resource.
	</p>
	<Button size="sm" onclick={openCreate} data-testid="server-create"><Plus class="size-4" /> Add server</Button>
</div>

{#if error}
	<div
		class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-200"
	>
		{error}
	</div>
{/if}

{#if loading}
	<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">Loading…</div>
{:else}
	<!-- Registered servers -->
	{#if servers.length === 0}
		<div
			class="rounded-xl border border-dashed border-border py-12 text-center text-sm text-muted-foreground"
		>
			No file servers registered yet.
		</div>
	{:else}
		<div class="space-y-2">
			{#each servers as s (s.key)}
				<div
					class="rounded-lg border bg-card transition-colors {focusKey === s.key
						? 'border-primary ring-1 ring-primary/30'
						: 'border-border'}"
					data-testid="server-row"
				>
					<!-- Server identity header -->
					<div class="flex items-center gap-3 px-4 py-3">
						<Server class="size-5 shrink-0 text-muted-foreground" />
						<div class="min-w-0 flex-1">
							<div class="flex items-center gap-2">
								<span class="truncate font-medium text-foreground">{s.display_name}</span>
								<span class="size-2 rounded-full {statusDot(s.status)}" title={s.status}></span>
							</div>
							<div class="mt-0.5 font-mono text-sm text-muted-foreground">{s.key}</div>
						</div>
						<div class="shrink-0 text-right">
							<div class="text-sm font-semibold tabular-nums text-foreground">
								{s.file_count.toLocaleString()} files
							</div>
							<div class="text-sm tabular-nums text-muted-foreground">
								{formatBytes(s.total_size_bytes)}
							</div>
						</div>
						<div class="flex shrink-0 items-center gap-1">
							<Button
								variant="ghost"
								size="icon-sm"
								onclick={() => openAddEndpoint(s)}
								title="Add endpoint"
								data-testid="endpoint-add"><Plus class="size-4" /></Button
							>
							<Button variant="ghost" size="icon-sm" onclick={() => openEditServer(s)} title="Edit server"
								><Pencil class="size-4" /></Button
							>
							<Button variant="ghost" size="icon-sm" onclick={() => removeServer(s)} title="Delete server"
								><Trash2 class="size-4" /></Button
							>
						</div>
					</div>

					<!-- Endpoint fan-out -->
					{#if s.endpoints.length === 0}
						<div class="border-t border-border px-4 py-2.5 text-sm text-muted-foreground">
							No endpoints — add one to reach this backend.
						</div>
					{:else}
						<div class="divide-y divide-border border-t border-border">
							{#each s.endpoints as ep (ep.id)}
								{@const MIcon = methodIcon(ep.access_method)}
								{@const ver = verificationVariant(ep.verification_status)}
								<div class="flex items-center gap-3 px-4 py-2 pl-8" data-testid="endpoint-row">
									<span class="size-2 shrink-0 rounded-full {statusDot(ep.status)}" title={ep.status}
									></span>
									<MIcon class="size-4 shrink-0 text-muted-foreground" />
									<Badge variant="outline" class="px-1.5 py-0 text-[10px]">{ep.access_method}</Badge>
									<span class="min-w-0 flex-1 truncate font-mono text-sm text-foreground">
										/{(ep.root ?? '').replace(/^\/+/, '')}
									</span>
									{#if ep.resource_ref}
										<span
											class="inline-flex items-center gap-1 text-sm text-muted-foreground"
											class:text-amber-600={unresolvedRef(s, ep)}
											title={unresolvedRef(s, ep) ? 'resource not found' : 'resource'}
										>
											{#if unresolvedRef(s, ep)}<Link2Off class="size-3" />{/if}
											→ {ep.resource_ref}
										</span>
									{/if}
									<Badge variant="outline" class="px-1.5 py-0 text-[10px] {ver.class}"
										>{ver.label}</Badge
									>
									<Button
										variant="ghost"
										size="icon-sm"
										disabled={verifying.has(ep.id)}
										onclick={() => runVerify(s, ep)}
										title="Verify endpoint (hash-probe a sample of this server's files through it)"
										data-testid="endpoint-verify"
									>
										{#if verifying.has(ep.id)}
											<LoaderCircle class="size-3.5 animate-spin" />
										{:else}
											<ShieldCheck class="size-3.5" />
										{/if}
									</Button>
									<Button
										variant="ghost"
										size="icon-sm"
										onclick={() => openEditEndpoint(s, ep)}
										title="Edit endpoint"><Pencil class="size-3.5" /></Button
									>
									<Button
										variant="ghost"
										size="icon-sm"
										onclick={() => removeEndpoint(s, ep)}
										title="Remove endpoint"><Trash2 class="size-3.5" /></Button
									>
								</div>
							{/each}
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}

	<!-- Unregistered inventory keys (adopt candidates) -->
	{#if unregistered.length > 0}
		<div class="mt-6">
			<h3 class="mb-2 text-sm font-semibold uppercase tracking-wider text-muted-foreground">
				Unregistered ({unregistered.length})
			</h3>
			<p class="mb-2 text-sm text-muted-foreground">
				Server names seen in inventory with no entity yet — adopt to manage and crawl them.
			</p>
			<div class="space-y-1.5">
				{#each unregistered as u (u.key)}
					<div
						class="flex items-center gap-3 rounded-lg border border-dashed px-4 py-2.5 {focusKey ===
						u.key
							? 'border-primary ring-1 ring-primary/30'
							: 'border-border'}"
						data-testid="server-unregistered"
					>
						<Server class="size-4 shrink-0 text-muted-foreground/60" />
						<span class="min-w-0 flex-1 truncate font-mono text-sm text-foreground">{u.key}</span>
						<span class="shrink-0 text-sm tabular-nums text-muted-foreground"
							>{u.file_count.toLocaleString()} files · {formatBytes(u.total_size_bytes)}</span
						>
						<Button variant="outline" size="sm" onclick={() => openAdopt(u)}>Adopt</Button>
					</div>
				{/each}
			</div>
		</div>
	{/if}
{/if}

<!-- Server create / adopt / edit dialog -->
<Dialog.Root bind:open={serverFormOpen}>
	<Dialog.Content class="sm:max-w-md">
		<Dialog.Header>
			<Dialog.Title
				>{serverFormMode === 'edit'
					? 'Edit file server'
					: serverFormMode === 'adopt'
						? 'Adopt file server'
						: 'Add file server'}</Dialog.Title
			>
		</Dialog.Header>
		<div class="space-y-3">
			<div>
				<!-- svelte-ignore a11y_label_has_associated_control -->
				<label class="mb-1 block text-sm font-medium text-foreground">Key</label>
				<Input
					bind:value={fKey}
					disabled={serverFormMode !== 'create'}
					placeholder="lab-nas-1"
					class="text-sm"
				/>
				<p class="mt-1 text-xs text-muted-foreground">
					Stable id; matches the inventory <code>file_server_id</code>.
				</p>
			</div>
			<div>
				<!-- svelte-ignore a11y_label_has_associated_control -->
				<label class="mb-1 block text-sm font-medium text-foreground">Display name</label>
				<Input bind:value={fDisplayName} placeholder="Lab NAS" class="text-sm" />
			</div>

			{#if serverFormMode !== 'edit'}
				<div class="rounded-lg border border-border p-3">
					<p class="mb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						First endpoint
					</p>
					<div class="space-y-3">
						<div>
							<!-- svelte-ignore a11y_label_has_associated_control -->
							<label class="mb-1 block text-sm font-medium text-foreground">Access method</label>
							<Select.Root
								type="single"
								value={fMethod}
								onValueChange={(v) => {
									if (v) {
										fMethod = v;
										fResourceRef = '';
										loadResourcesForMethod(v);
									}
								}}
							>
								<Select.Trigger class="text-sm">{fMethod}</Select.Trigger>
								<Select.Content>
									{#each accessMethods as m}<Select.Item value={m} label={m} />{/each}
								</Select.Content>
							</Select.Root>
						</div>
						{#if methodNeedsResource(fMethod)}
							<div>
								<!-- svelte-ignore a11y_label_has_associated_control -->
								<label class="mb-1 block text-sm font-medium text-foreground"
									>Resource ({fMethod} credentials)</label
								>
								{#if resources.length > 0}
									<Select.Root
										type="single"
										value={fResourceRef}
										onValueChange={(v) => (fResourceRef = v ?? '')}
									>
										<Select.Trigger class="text-sm"
											>{fResourceRef || 'Select a resource…'}</Select.Trigger
										>
										<Select.Content>
											{#each resources as r}<Select.Item
													value={r.path}
													label={`${r.display_name} (${r.path})`}
												/>{/each}
										</Select.Content>
									</Select.Root>
								{:else}
									<Input
										bind:value={fResourceRef}
										placeholder="resource path (e.g. lab_nas)"
										class="text-sm"
									/>
									<p class="mt-1 text-xs text-muted-foreground">
										No <code>{fMethod}</code> resources found — create one under Resources, or type its
										path.
									</p>
								{/if}
							</div>
						{/if}
						{#if fMethod === 'local_mount'}
							{@const fGroupEmpty =
								serverFormMode === 'adopt' ? 'Auto-detect from registration' : 'None'}
							<div>
								<!-- svelte-ignore a11y_label_has_associated_control -->
								<label class="mb-1 block text-sm font-medium text-foreground"
									>Serving pool / runner
									<span class="font-normal text-muted-foreground">(optional)</span></label
								>
								<Select.Root
									type="single"
									value={fGroupId || '__none__'}
									onValueChange={(v) => (fGroupId = !v || v === '__none__' ? '' : v)}
								>
									<Select.Trigger class="text-sm" data-testid="group-select"
										>{groupLabelFor(fGroupId, fGroupEmpty)}</Select.Trigger
									>
									<Select.Content>
										<Select.Item value="__none__" label={fGroupEmpty} />
										{#each groupOptions as o}<Select.Item value={o.value} label={o.label} />{/each}
									</Select.Content>
								</Select.Root>
								<p class="mt-1 text-xs text-muted-foreground">
									Whose co-located workers serve this mount{serverFormMode === 'adopt'
										? ' — files registered by a runner carry this automatically'
										: ''}.
								</p>
							</div>
						{/if}
						<div>
							<!-- svelte-ignore a11y_label_has_associated_control -->
							<label class="mb-1 block text-sm font-medium text-foreground"
								>Root <span class="font-normal text-muted-foreground">(optional)</span></label
							>
							<Input bind:value={fRoot} placeholder="legacy/datasets" class="text-sm" />
						</div>
					</div>
				</div>
			{/if}

			{#if serverFormError}<p class="text-sm text-red-600">{serverFormError}</p>{/if}
		</div>
		<Dialog.Footer>
			<Button variant="ghost" onclick={() => (serverFormOpen = false)}>Cancel</Button>
			<Button onclick={saveServer} disabled={savingServer || !fKey}
				>{savingServer ? 'Saving…' : 'Save'}</Button
			>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>

<!-- Endpoint add / edit dialog -->
<Dialog.Root bind:open={epFormOpen}>
	<Dialog.Content class="sm:max-w-md">
		<Dialog.Header>
			<Dialog.Title>{epFormMode === 'edit' ? 'Edit endpoint' : 'Add endpoint'}</Dialog.Title>
		</Dialog.Header>
		<div class="space-y-3">
			<div>
				<!-- svelte-ignore a11y_label_has_associated_control -->
				<label class="mb-1 block text-sm font-medium text-foreground">Access method</label>
				<Select.Root
					type="single"
					value={epMethod}
					onValueChange={(v) => {
						if (v) {
							epMethod = v;
							epResourceRef = '';
							loadResourcesForMethod(v);
						}
					}}
				>
					<Select.Trigger class="text-sm">{epMethod}</Select.Trigger>
					<Select.Content>
						{#each accessMethods as m}<Select.Item value={m} label={m} />{/each}
					</Select.Content>
				</Select.Root>
			</div>
			{#if methodNeedsResource(epMethod)}
				<div>
					<!-- svelte-ignore a11y_label_has_associated_control -->
					<label class="mb-1 block text-sm font-medium text-foreground"
						>Resource ({epMethod} credentials)</label
					>
					{#if resources.length > 0}
						<Select.Root
							type="single"
							value={epResourceRef}
							onValueChange={(v) => (epResourceRef = v ?? '')}
						>
							<Select.Trigger class="text-sm">{epResourceRef || 'Select a resource…'}</Select.Trigger>
							<Select.Content>
								{#each resources as r}<Select.Item
										value={r.path}
										label={`${r.display_name} (${r.path})`}
									/>{/each}
							</Select.Content>
						</Select.Root>
					{:else}
						<Input
							bind:value={epResourceRef}
							placeholder="resource path (e.g. lab_nas)"
							class="text-sm"
						/>
						<p class="mt-1 text-xs text-muted-foreground">
							No <code>{epMethod}</code> resources found — create one under Resources, or type its
							path.
						</p>
					{/if}
				</div>
			{/if}
			{#if epMethod === 'local_mount'}
				<div>
					<!-- svelte-ignore a11y_label_has_associated_control -->
					<label class="mb-1 block text-sm font-medium text-foreground"
						>Serving pool / runner
						<span class="font-normal text-muted-foreground">(optional)</span></label
					>
					<Select.Root
						type="single"
						value={epGroupId || '__none__'}
						onValueChange={(v) => (epGroupId = !v || v === '__none__' ? '' : v)}
					>
						<Select.Trigger class="text-sm" data-testid="group-select"
							>{groupLabelFor(epGroupId, 'None')}</Select.Trigger
						>
						<Select.Content>
							<Select.Item value="__none__" label="None" />
							{#each groupOptions as o}<Select.Item value={o.value} label={o.label} />{/each}
						</Select.Content>
					</Select.Root>
					<p class="mt-1 text-xs text-muted-foreground">
						Whose co-located workers serve this mount. Required for a local mount to be
						servable.
					</p>
				</div>
			{/if}
			<div>
				<!-- svelte-ignore a11y_label_has_associated_control -->
				<label class="mb-1 block text-sm font-medium text-foreground"
					>Root <span class="font-normal text-muted-foreground">(optional)</span></label
				>
				<Input bind:value={epRoot} placeholder="legacy/datasets" class="text-sm" />
			</div>
			<div>
				<!-- svelte-ignore a11y_label_has_associated_control -->
				<label class="mb-1 block text-sm font-medium text-foreground"
					>Priority <span class="font-normal text-muted-foreground">(higher = preferred)</span></label
				>
				<Input bind:value={epPriority} type="number" placeholder="0" class="text-sm" />
			</div>
			{#if epFormError}<p class="text-sm text-red-600">{epFormError}</p>{/if}
		</div>
		<Dialog.Footer>
			<Button variant="ghost" onclick={() => (epFormOpen = false)}>Cancel</Button>
			<Button onclick={saveEndpoint} disabled={savingEp}>{savingEp ? 'Saving…' : 'Save'}</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
