<script lang="ts">
	// Top-level asset browser (docs/20 §3/§4). Two stacked sections:
	//   1. Asset TYPES — the user-defined schemas (define / edit / delete).
	//   2. ASSETS — typed collections, grouped by virtual folder (display_path
	//      prefix). A scope selector drives downward-visibility resolution and
	//      the owner-scope for new types/assets.
	//
	// Mirrors `resources/ResourceList.svelte` (filter + list rows + edit modal),
	// extended with the scope selector + folder grouping the asset layer needs.
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import * as Select from '$lib/components/ui/select';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Boxes from '@lucide/svelte/icons/boxes';
	import Database from '@lucide/svelte/icons/database';
	import Upload from '@lucide/svelte/icons/upload';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import Folder from '@lucide/svelte/icons/folder';
	import {
		listAssetTypes,
		listAssets,
		deleteAssetType,
		deleteAsset,
		createAsset,
		type AssetTypeSummary,
		type AssetSummary,
		type ScopeContext
	} from '$lib/api/assets';
	import ScopeSelector from './ScopeSelector.svelte';
	import AssetTypeBuilder from './AssetTypeBuilder.svelte';
	import AssetEditor from './AssetEditor.svelte';
	import CsvImport from './CsvImport.svelte';

	let scope = $state<ScopeContext>({ kind: 'workspace' });
	let types = $state<AssetTypeSummary[]>([]);
	let assets = $state<AssetSummary[]>([]);
	let typeFilter = $state<string>('');
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Modal state.
	let typeBuilderOpen = $state(false);
	let editingTypeId = $state<string | null>(null);
	let editorOpen = $state(false);
	let editingAsset = $state<AssetSummary | null>(null);
	let csvOpen = $state(false);
	let csvAsset = $state<AssetSummary | null>(null);

	// Collapsed virtual folders (by path prefix).
	let collapsed = $state<Record<string, boolean>>({});

	const scopeKey = $derived.by(() => {
		if (scope.kind === 'workspace') return 'workspace';
		return `${scope.kind}:${scope.id}`;
	});

	async function load() {
		loading = true;
		error = null;
		try {
			const [t, a] = await Promise.all([
				listAssetTypes({ scope }),
				listAssets({ scope, type_id: typeFilter || undefined })
			]);
			types = t.items;
			assets = a.items;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load assets';
			types = [];
			assets = [];
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void scopeKey;
		void typeFilter;
		load();
	});

	function typeName(id: string): string {
		return types.find((t) => t.id === id)?.name ?? id.slice(0, 8);
	}

	// Group assets by their display_path folder prefix (everything before the
	// last `/`). Assets without a display_path land in the root "" group.
	type FolderGroup = { folder: string; assets: AssetSummary[] };
	const grouped = $derived.by<FolderGroup[]>(() => {
		const map = new Map<string, AssetSummary[]>();
		for (const a of assets) {
			const folder = a.display_path ?? '';
			if (!map.has(folder)) map.set(folder, []);
			map.get(folder)!.push(a);
		}
		return [...map.entries()]
			.sort(([x], [y]) => x.localeCompare(y))
			.map(([folder, list]) => ({ folder, assets: list }));
	});

	function openCreateType() {
		editingTypeId = null;
		typeBuilderOpen = true;
	}
	function openEditType(id: string) {
		editingTypeId = id;
		typeBuilderOpen = true;
	}
	function openEditAsset(a: AssetSummary) {
		editingAsset = a;
		editorOpen = true;
	}
	function openCsv(a: AssetSummary) {
		csvAsset = a;
		csvOpen = true;
	}

	async function handleDeleteType(t: AssetTypeSummary) {
		if (!confirm(`Soft-delete asset type "${t.name}"? Rejected if assets of this type still exist.`)) return;
		try {
			await deleteAssetType(t.id);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to delete type';
		}
	}

	async function handleDeleteAsset(a: AssetSummary) {
		if (!confirm(`Soft-delete asset "${a.ref_key}"? Pinned instances keep their pinned version.`)) return;
		try {
			await deleteAsset(a.id);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to delete asset';
		}
	}

	// New asset: prompt for a ref_key, mint against the current type filter (or
	// the first available type), open the records editor.
	async function newAsset() {
		const targetType = typeFilter || types[0]?.id;
		if (!targetType) {
			error = 'Define an asset type first.';
			return;
		}
		const refKey = prompt('Asset ref-key (lowercase identifier, e.g. steel):')?.trim();
		if (!refKey) return;
		if (!/^[a-z][a-z0-9_]*$/.test(refKey)) {
			error = 'Ref-key must be a lowercase identifier (^[a-z][a-z0-9_]*$).';
			return;
		}
		try {
			const created = await createAsset({
				type_id: targetType,
				ref_key: refKey,
				display_name: refKey,
				scope_kind: scope.kind,
				scope_id: scope.kind === 'workspace' ? null : scope.id
			});
			await load();
			openEditAsset(created);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to create asset';
		}
	}

	function onTypeSaved() {
		typeBuilderOpen = false;
		editingTypeId = null;
		load();
	}
	function onAssetSaved() {
		editorOpen = false;
		csvOpen = false;
		editingAsset = null;
		csvAsset = null;
		load();
	}

	function toggleFolder(folder: string) {
		collapsed = { ...collapsed, [folder]: !collapsed[folder] };
	}

	const formatDate = (s: string) => new Date(s).toLocaleString();
</script>

<div class="space-y-6" data-testid="assets-list">
	<div class="flex flex-wrap items-center gap-3">
		<ScopeSelector value={scope} onChange={(s) => (scope = s)} />
		<div class="flex items-center gap-2">
			<span class="text-sm font-medium text-muted-foreground">Type</span>
			<Select.Root type="single" value={typeFilter} onValueChange={(v) => (typeFilter = v ?? '')}>
				<Select.Trigger class="h-9 min-w-[160px]">
					{typeFilter ? typeName(typeFilter) : 'All types'}
				</Select.Trigger>
				<Select.Content>
					<Select.Item value="" label="All types" />
					{#each types as t (t.id)}
						<Select.Item value={t.id} label={t.display_name} />
					{/each}
				</Select.Content>
			</Select.Root>
		</div>
	</div>

	{#if error}
		<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{/if}

	<!-- Asset types section -->
	<section class="space-y-3">
		<div class="flex items-center justify-between">
			<div class="flex items-center gap-2">
				<Boxes class="size-4 text-muted-foreground" />
				<h2 class="text-base font-semibold">Asset types</h2>
			</div>
			<Button variant="outline" size="sm" class="gap-1.5" onclick={openCreateType} data-testid="asset-type-create">
				<Plus class="size-4" />
				New type
			</Button>
		</div>
		{#if loading}
			<p class="py-4 text-center text-sm text-muted-foreground">Loading…</p>
		{:else if types.length === 0}
			<p class="rounded-xl border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground">
				No asset types defined in this scope. Define a schema (a list of typed fields) to start curating content.
			</p>
		{:else}
			<div class="space-y-2">
				{#each types as t (t.id)}
					<div class="group flex items-center justify-between rounded-lg border border-border bg-card p-3 transition-colors hover:bg-accent/40">
						<button type="button" class="flex min-w-0 flex-1 items-center gap-3 text-left" onclick={() => openEditType(t.id)}>
							<Boxes class="size-4 shrink-0 text-muted-foreground" />
							<div class="min-w-0">
								<div class="flex flex-wrap items-center gap-2">
									<span class="font-mono text-sm font-medium">{t.name}</span>
									<Badge variant="secondary">{t.cardinality}</Badge>
									<Badge variant="outline">v{t.version}</Badge>
									{#if t.display_path}<Badge variant="outline" class="font-mono">{t.display_path}</Badge>{/if}
								</div>
								<p class="mt-0.5 truncate text-sm text-muted-foreground">{t.display_name}</p>
							</div>
						</button>
						<div class="flex shrink-0 items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100">
							<Button variant="ghost" size="sm" class="gap-1 text-sm text-muted-foreground" onclick={() => openEditType(t.id)}>
								<Pencil class="size-3.5" /> Edit
							</Button>
							<Button
								variant="ghost"
								size="sm"
								class="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
								onclick={() => handleDeleteType(t)}
								title="Soft-delete"
							>
								<Trash2 class="size-3.5" />
							</Button>
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</section>

	<!-- Assets section, grouped by virtual folder -->
	<section class="space-y-3">
		<div class="flex items-center justify-between">
			<div class="flex items-center gap-2">
				<Database class="size-4 text-muted-foreground" />
				<h2 class="text-base font-semibold">Assets</h2>
			</div>
			<Button variant="default" size="sm" class="gap-1.5" onclick={newAsset} data-testid="asset-create" disabled={types.length === 0}>
				<Plus class="size-4" />
				New asset
			</Button>
		</div>

		{#if loading}
			<p class="py-4 text-center text-sm text-muted-foreground">Loading…</p>
		{:else if assets.length === 0}
			<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-12">
				<Database class="size-9 text-muted-foreground/40" />
				<p class="mt-3 text-sm text-muted-foreground">No assets in this scope</p>
				<p class="text-sm text-muted-foreground">Curated content collections — a materials table, a script library, reference data.</p>
			</div>
		{:else}
			<div class="space-y-3">
				{#each grouped as group (group.folder)}
					<div>
						{#if group.folder}
							<button
								type="button"
								class="mb-1.5 flex items-center gap-1.5 text-sm font-medium text-muted-foreground hover:text-foreground"
								onclick={() => toggleFolder(group.folder)}
							>
								{#if collapsed[group.folder]}
									<ChevronRight class="size-4" />
								{:else}
									<ChevronDown class="size-4" />
								{/if}
								<Folder class="size-3.5" />
								<span class="font-mono">{group.folder}</span>
								<Badge variant="secondary">{group.assets.length}</Badge>
							</button>
						{/if}
						{#if !group.folder || !collapsed[group.folder]}
							<div class="space-y-2 {group.folder ? 'ml-5' : ''}">
								{#each group.assets as a (a.id)}
									<div class="group flex items-center justify-between rounded-lg border border-border bg-card p-3 transition-colors hover:bg-accent/40" data-testid="asset-item-{a.id}">
										<button type="button" class="flex min-w-0 flex-1 items-center gap-3 text-left" onclick={() => openEditAsset(a)}>
											<Database class="size-4 shrink-0 text-muted-foreground" />
											<div class="min-w-0">
												<div class="flex flex-wrap items-center gap-2">
													<span class="font-mono text-sm font-medium">{a.ref_key}</span>
													<Badge variant="secondary">{typeName(a.type_id)}</Badge>
													<Badge variant="outline">v{a.version}</Badge>
												</div>
												<p class="mt-0.5 truncate text-sm text-muted-foreground">{a.display_name}</p>
												<p class="mt-0.5 text-sm text-muted-foreground">Updated {formatDate(a.updated_at)}</p>
											</div>
										</button>
										<div class="flex shrink-0 items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100">
											<Button variant="ghost" size="sm" class="gap-1 text-sm text-muted-foreground" onclick={() => openCsv(a)} title="Import CSV">
												<Upload class="size-3.5" /> CSV
											</Button>
											<Button variant="ghost" size="sm" class="gap-1 text-sm text-muted-foreground" onclick={() => openEditAsset(a)}>
												<Pencil class="size-3.5" /> Edit
											</Button>
											<Button
												variant="ghost"
												size="sm"
												class="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
												onclick={() => handleDeleteAsset(a)}
												title="Soft-delete"
											>
												<Trash2 class="size-3.5" />
											</Button>
										</div>
									</div>
								{/each}
							</div>
						{/if}
					</div>
				{/each}
			</div>
		{/if}
	</section>
</div>

<AssetTypeBuilder bind:open={typeBuilderOpen} typeId={editingTypeId} {scope} onsaved={onTypeSaved} />
<AssetEditor bind:open={editorOpen} asset={editingAsset} onsaved={onAssetSaved} />
<CsvImport bind:open={csvOpen} asset={csvAsset} onsaved={onAssetSaved} />
