<script lang="ts">
	// Top-level resource list. Renders the workspace's resources (one row
	// each) with Create + Delete affordances. The schema-driven create
	// flow lives in `ResourceEditModal.svelte`; this component owns the
	// list, filter dropdown, and refresh — not the form.
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import * as Select from '$lib/components/ui/select';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import Share2 from '@lucide/svelte/icons/share-2';
	import Lock from '@lucide/svelte/icons/lock';
	import {
		deleteResource,
		listResources,
		listResourceTypes,
		type ResourceSummary,
		type ResourceTypeInfo
	} from '$lib/api/resources';
	import { roleAtLeast } from '$lib/api/iam';
	import { isPlatformResource } from '$lib/api/resource-tier';
	import Globe from '@lucide/svelte/icons/globe';
	import AuthorshipChips from '$lib/components/iam/AuthorshipChips.svelte';
	import ShareDialog from '$lib/components/iam/ShareDialog.svelte';
	import ResourceEditModal from './ResourceEditModal.svelte';

	type Props = {
		workspace_id?: string;
		/** When set, scope the list + default new-resource placement to this
		 *  folder (the folders-browser Resources tab). */
		folderId?: string;
	};

	let { workspace_id, folderId }: Props = $props();

	// Resource types the Control Plane (/fleet) now owns — hidden here so the
	// two surfaces don't both list them. Client-side filter only; the shared
	// listResources endpoint is untouched.
	const CONTROL_PLANE_TYPES = new Set(['capacity', 'datacenter']);

	let resources = $state<ResourceSummary[]>([]);
	let types = $state<ResourceTypeInfo[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let typeFilter = $state<string>('');

	// Rows + dropdown options with the Control-Plane-owned types stripped out.
	const visibleResources = $derived(
		resources.filter((r) => !CONTROL_PLANE_TYPES.has(r.resource_type))
	);
	const visibleTypes = $derived(types.filter((t) => !CONTROL_PLANE_TYPES.has(t.name)));

	let editorOpen = $state(false);
	let editingId = $state<string | null>(null);

	// Object-grant Share dialog (object-Admins only; the per-row button is gated
	// on the row's my_effective_role).
	let shareOpen = $state(false);
	let shareResource = $state<ResourceSummary | null>(null);
	function openShare(r: ResourceSummary) {
		shareResource = r;
		shareOpen = true;
	}

	async function load() {
		loading = true;
		error = null;
		try {
			const [p, t] = await Promise.all([
				listResources({
					resource_type: typeFilter || undefined,
					workspace_id,
					// Exact placement: a folder shows only resources placed in it; the
					// root (workspace) shows only workspace-scoped resources.
					scope: folderId ? `folder:${folderId}` : 'workspace',
					exact: true,
					perPage: 200
				}),
				types.length === 0 ? listResourceTypes() : Promise.resolve(types)
			]);
			resources = p.items;
			if (types.length === 0) types = t;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load resources';
			resources = [];
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void typeFilter;
		void workspace_id;
		void folderId;
		load();
	});

	function openCreate() {
		editingId = null;
		editorOpen = true;
	}

	function openEdit(id: string) {
		editingId = id;
		editorOpen = true;
	}

	async function handleDelete(id: string, path: string) {
		if (!confirm(`Soft-delete resource "${path}"? Existing pinned instances keep resolving against their pinned version.`)) return;
		try {
			await deleteResource(id);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to delete';
		}
	}

	function onSaved() {
		editorOpen = false;
		editingId = null;
		load();
	}

	const typeIcon: Record<string, string> = {
		postgres: '🐘',
		openai: '✨',
		slack: '💬',
		s3: '📦',
		smtp: '✉️',
		google_oauth: '🔑'
	};
</script>

<div class="space-y-4" data-testid="resources-list">
	<div class="flex flex-wrap items-center gap-3">
		<div class="flex items-center gap-2">
			<span class="text-sm font-medium text-muted-foreground">Type</span>
			<Select.Root
				type="single"
				value={typeFilter}
				onValueChange={(v) => (typeFilter = v ?? '')}
			>
				<Select.Trigger class="h-9 min-w-[160px]">
					{typeFilter
						? (visibleTypes.find((t) => t.name === typeFilter)?.display_name ?? typeFilter)
						: 'All types'}
				</Select.Trigger>
				<Select.Content>
					<Select.Item value="" label="All types" />
					{#each visibleTypes as t (t.name)}
						<Select.Item value={t.name} label={t.display_name} />
					{/each}
				</Select.Content>
			</Select.Root>
		</div>
		<Button
			variant="default"
			size="sm"
			onclick={openCreate}
			class="ml-auto gap-1.5"
			data-testid="resource-create-button"
		>
			<Plus class="size-4" />
			New resource
		</Button>
	</div>

	{#if error}
		<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{/if}

	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
			Loading…
		</div>
	{:else if visibleResources.length === 0}
		<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
			<KeyRound class="size-10 text-muted-foreground/40" />
			<p class="mt-3 text-sm text-muted-foreground">No resources defined</p>
			<p class="text-sm text-muted-foreground">
				Resources are typed credentials (Postgres, OpenAI, …) workflows bind by alias at launch.
			</p>
			<Button variant="outline" size="sm" class="mt-4 gap-1.5" onclick={openCreate}>
				<Plus class="size-4" />
				Create your first resource
			</Button>
		</div>
	{:else}
		<div class="space-y-2">
			{#each visibleResources as r (r.id)}
				{@const isPlatform = isPlatformResource(r)}
				{@const canEdit = roleAtLeast(r.my_effective_role, 'editor')}
				{@const canShare = roleAtLeast(r.my_effective_role, 'admin')}
				<div
					class="group flex items-center justify-between rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent/40"
					data-testid="resource-item-{r.id}"
				>
					<button
						type="button"
						class="flex min-w-0 flex-1 items-start gap-3 text-left"
						onclick={() => openEdit(r.id)}
					>
						<span class="mt-0.5 text-base leading-none">{typeIcon[r.resource_type] ?? '🔐'}</span>
						<div class="min-w-0 flex-1">
							<div class="flex flex-wrap items-center gap-2">
								<span class="font-mono text-sm font-medium text-foreground">{r.path}</span>
								<Badge variant="secondary">{r.resource_type}</Badge>
								<Badge variant="outline">v{r.latest_version}</Badge>
								{#if isPlatform}
									<Badge
										class="gap-1 bg-sky-100 text-sky-800"
										variant="secondary"
										title="Platform tier — shared across all workspaces, managed by platform admins"
										data-testid="resource-platform-badge-{r.id}"
									>
										<Globe class="size-3" /> Platform
									</Badge>
								{/if}
								{#if r.restricted}
									<Badge
										class="gap-1 bg-amber-100 text-amber-800"
										variant="secondary"
										title="Private — access by grant only, not shared workspace-wide"
									>
										<Lock class="size-3" /> Private
									</Badge>
								{/if}
							</div>
							<p class="mt-1 truncate text-sm text-muted-foreground">{r.display_name}</p>
							<AuthorshipChips
								class="mt-1"
								createdBy={r.created_by}
								createdAt={r.created_at}
								updatedBy={r.updated_by}
								updatedAt={r.updated_at}
							/>
						</div>
					</button>
					<div class="flex shrink-0 items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100">
						{#if canShare}
							<Button
								variant="ghost"
								size="sm"
								class="gap-1 text-sm text-muted-foreground"
								onclick={() => openShare(r)}
								title="Share / manage access"
								data-testid="resource-share-{r.id}"
							>
								<Share2 class="size-3.5" />
								Share
							</Button>
						{/if}
						{#if canEdit}
							<Button
								variant="ghost"
								size="sm"
								class="gap-1 text-sm text-muted-foreground"
								onclick={() => openEdit(r.id)}
								title="Edit / rotate"
							>
								<RotateCcw class="size-3.5" />
								Edit
							</Button>
							<Button
								variant="ghost"
								size="sm"
								class="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
								onclick={() => handleDelete(r.id, r.path)}
								title="Soft-delete"
							>
								<Trash2 class="size-3.5" />
							</Button>
						{/if}
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>

<ResourceEditModal
	bind:open={editorOpen}
	resource_id={editingId}
	{types}
	{workspace_id}
	defaultFolderId={folderId}
	onsaved={onSaved}
	onmoved={load}
/>

{#if shareResource}
	<ShareDialog
		bind:open={shareOpen}
		objectType="resource"
		objectId={shareResource.id}
		objectName={shareResource.path}
		myEffectiveRole={shareResource.my_effective_role}
		onChanged={load}
	/>
{/if}
