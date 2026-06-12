<script lang="ts">
	// Object/table builder for an asset's records (docs/20 §4.2). For a
	// `collection` type it renders an editable grid (add/edit/delete rows); for
	// an `object` type a single-row form (the 1-row degenerate case). Each cell
	// renders through the shared FieldWidget. File fields support both dual
	// sources (§4.1): upload (→ S3) and pick-from-catalog (reuse storage_path).
	//
	// Editing rows and saving bumps the asset version server-side (PUT records).
	import { Sheet, SheetContent, SheetTitle, SheetDescription, SheetClose } from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import FieldWidget from '$lib/fields/FieldWidget.svelte';
	import * as FileDropZone from '$lib/components/ui/file-drop-zone';
	import X from '@lucide/svelte/icons/x';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Upload from '@lucide/svelte/icons/upload';
	import FolderOpen from '@lucide/svelte/icons/folder-open';
	import CatalogFilePicker from './CatalogFilePicker.svelte';
	import { specFromPortField, emptyRecord, buildRecord, displayCell } from './field-spec';
	import { fromPortFieldKind } from '$lib/fields/adapters';
	import Activity from '@lucide/svelte/icons/activity';
	import {
		getAsset,
		getAssetType,
		getAssetUsage,
		putAssetRecords,
		uploadAssetFile,
		moveAsset,
		type AssetSummary,
		type AssetTypeDetail,
		type PaginatedAssetUsage,
		type PortField,
		type ScopeContext
	} from '$lib/api/assets';
	import { roleAtLeast } from '$lib/api/iam';
	import MoveLocationField from '$lib/components/iam/MoveLocationField.svelte';

	type Props = {
		open: boolean;
		/** The asset to edit. */
		asset: AssetSummary | null;
		onsaved: () => void;
		/** Refresh the underlying list after a scope move WITHOUT closing the
		 *  sheet (unlike `onsaved`, which closes). Defaults to `onsaved`. */
		onmoved?: () => void;
	};

	let { open = $bindable(), asset, onsaved, onmoved }: Props = $props();

	// The asset's owner scope, as a ScopeContext for the move control. Seeded
	// from the asset and updated optimistically on a successful move.
	function scopeOf(a: AssetSummary | null): ScopeContext {
		if (!a || a.scope_kind === 'workspace') return { kind: 'workspace' };
		return { kind: a.scope_kind as 'folder' | 'template', id: a.scope_id };
	}
	// Seeded by the open-effect when an asset loads (avoids capturing only the
	// initial prop value); updated optimistically on a successful move.
	let currentScope = $state<ScopeContext>({ kind: 'workspace' });
	const canMove = $derived(roleAtLeast(asset?.my_effective_role, 'editor'));

	async function moveTo(next: ScopeContext) {
		if (!asset) return;
		await moveAsset(asset.id, next);
		currentScope = next;
		(onmoved ?? onsaved)();
	}

	let type = $state<AssetTypeDetail | null>(null);
	let rows = $state<Record<string, unknown>[]>([]);
	let loading = $state(false);
	let saving = $state(false);
	let error = $state<string | null>(null);
	let loadedFor = $state<string | null>(null);

	// Reverse lineage (docs/20 §9): runs that pinned this asset.
	let usage = $state<PaginatedAssetUsage | null>(null);
	let usageOpen = $state(false);

	// Catalog picker state — which (rowIndex, fieldName) is awaiting a pick.
	let catalogOpen = $state(false);
	let catalogTarget = $state<{ row: number; field: string } | null>(null);

	const fields = $derived<PortField[]>(type?.fields ?? []);
	const isObject = $derived(type?.cardinality === 'object');

	$effect(() => {
		if (!open || !asset) {
			loadedFor = null;
			return;
		}
		if (loadedFor === asset.id) return;
		loadedFor = asset.id;
		currentScope = scopeOf(asset);
		void bootstrap(asset);
	});

	async function bootstrap(a: AssetSummary) {
		loading = true;
		error = null;
		usage = null;
		usageOpen = false;
		try {
			const [t, detail] = await Promise.all([getAssetType(a.type_id), getAsset(a.id)]);
			type = t;
			// Reverse-lineage is best-effort — never block the editor on it.
			void getAssetUsage(a.id).then((u) => (usage = u)).catch(() => {});
			// Materialize records; seed at least one row for an object type.
			const loaded = (detail.records as Record<string, unknown>[]) ?? [];
			if (t.cardinality === 'object') {
				rows = loaded.length > 0 ? [loaded[0]] : [emptyRecord(t.fields)];
			} else {
				rows = loaded;
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load asset';
		} finally {
			loading = false;
		}
	}

	function addRow() {
		if (!type) return;
		rows = [...rows, emptyRecord(type.fields)];
	}

	function removeRow(i: number) {
		rows = rows.filter((_, idx) => idx !== i);
	}

	function setCell(i: number, field: string, value: unknown) {
		rows = rows.map((r, idx) => (idx === i ? { ...r, [field]: value } : r));
	}

	async function handleFileUpload(i: number, field: string, files: File[]) {
		if (!asset || files.length === 0) return;
		try {
			const res = await uploadAssetFile(asset.id, field, files[0]);
			setCell(i, field, res.storage_path);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Upload failed';
		}
	}

	function openCatalog(i: number, field: string) {
		catalogTarget = { row: i, field };
		catalogOpen = true;
	}

	function onCatalogPick(storagePath: string) {
		if (catalogTarget) setCell(catalogTarget.row, catalogTarget.field, storagePath);
		catalogTarget = null;
	}

	async function save() {
		if (!asset || !type) return;
		saving = true;
		error = null;
		try {
			const records = rows.map((r) => buildRecord(type!.fields, r));
			await putAssetRecords(asset.id, { records, append: false });
			onsaved();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Save failed';
		} finally {
			saving = false;
		}
	}

	function isFileField(f: PortField): boolean {
		return fromPortFieldKind(f.kind) === 'file';
	}
</script>

<Sheet.Root bind:open>
	<SheetContent class="w-[720px] sm:max-w-[720px]">
		<div class="flex items-center justify-between border-b border-border px-5 py-4">
			<div class="min-w-0">
				<SheetTitle class="text-lg font-semibold">
					{asset?.display_name ?? asset?.ref_key ?? 'Asset'}
				</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					{#if type}
						{isObject ? 'Single-record (object) asset' : 'Collection asset'} ·
						<span class="font-mono">{type.name}</span> · v{asset?.version}
					{:else}
						Loading…
					{/if}
				</SheetDescription>
			</div>
			<SheetClose>
				<X class="size-4" />
			</SheetClose>
		</div>

		<div class="flex flex-1 flex-col overflow-y-auto px-5 py-4">
			{#if error}
				<div class="mb-4 rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">
					{error}
				</div>
			{/if}

			{#if asset && canMove}
				<div class="mb-4">
					<MoveLocationField scope={currentScope} onMove={moveTo} testid="asset-move" />
				</div>
			{/if}

			{#if usage && usage.total > 0}
				<!-- Reverse lineage (docs/20 §9): runs that used this asset. -->
				<div class="mb-4 rounded-md border border-border bg-muted/30">
					<button
						type="button"
						class="flex w-full items-center gap-2 px-3 py-2 text-sm"
						onclick={() => (usageOpen = !usageOpen)}
					>
						<Activity class="size-4 text-muted-foreground" />
						<span class="font-medium">Used by {usage.total} run{usage.total === 1 ? '' : 's'}</span>
						<span class="ml-auto text-xs text-muted-foreground">{usageOpen ? 'Hide' : 'Show'}</span>
					</button>
					{#if usageOpen}
						<ul class="border-t border-border divide-y divide-border">
							{#each usage.items as run (run.instance_id)}
								<li class="px-3 py-2 text-sm">
									<a
										href={`/instances/${run.instance_id}`}
										class="flex items-center gap-2 hover:underline"
									>
										<span class="truncate font-medium">{run.template_name}</span>
										<Badge variant="outline" class="shrink-0 font-mono text-[10px]">{run.status}</Badge>
										<span class="ml-auto shrink-0 text-xs text-muted-foreground">
											{run.alias} · v{run.version_used}
										</span>
									</a>
								</li>
							{/each}
						</ul>
					{/if}
				</div>
			{/if}

			{#if loading || !type}
				<p class="py-12 text-center text-sm text-muted-foreground">Loading…</p>
			{:else if isObject}
				<!-- Object: single-record form -->
				<div class="space-y-4">
					{#each fields as field (field.name)}
						{@const spec = specFromPortField(field)}
						<div class="space-y-1.5">
							<div class="flex items-center gap-2">
								<span class="text-sm font-medium">{field.label}</span>
								{#if field.required}<Badge variant="outline">required</Badge>{/if}
							</div>
							{#if isFileField(field)}
								{@const current = rows[0]?.[field.name]}
								<div class="space-y-2">
									{#if typeof current === 'string' && current !== ''}
										<p class="truncate rounded-md bg-muted/50 px-2 py-1 font-mono text-xs">{current}</p>
									{/if}
									<div class="flex items-center gap-2">
										<FileDropZone.Root
											accept={field.accept ?? undefined}
											maxFiles={1}
											onUpload={(files) => handleFileUpload(0, field.name, files)}
										>
											<FileDropZone.Trigger>
												<span class="inline-flex items-center gap-1.5 text-sm"><Upload class="size-3.5" /> Upload</span>
											</FileDropZone.Trigger>
										</FileDropZone.Root>
										<Button variant="outline" size="sm" class="gap-1.5" onclick={() => openCatalog(0, field.name)}>
											<FolderOpen class="size-3.5" />
											Catalog
										</Button>
									</div>
								</div>
							{:else}
								<FieldWidget
									spec={spec}
									value={rows[0]?.[field.name]}
									booleanWidget="select"
									onchange={(v) => setCell(0, field.name, v)}
								/>
							{/if}
						</div>
					{/each}
				</div>
			{:else}
				<!-- Collection: grid of rows -->
				<div class="space-y-3">
					<div class="flex items-center justify-between">
						<span class="text-sm text-muted-foreground">{rows.length} record{rows.length === 1 ? '' : 's'}</span>
						<Button variant="outline" size="sm" class="h-7 gap-1 px-2 text-sm" onclick={addRow}>
							<Plus class="size-3.5" /> Add row
						</Button>
					</div>

					{#if rows.length === 0}
						<p class="rounded-md border border-dashed border-border px-3 py-8 text-center text-sm text-muted-foreground">
							No records yet. Add a row, or import a CSV.
						</p>
					{:else}
						<div class="space-y-3">
							{#each rows as row, i (i)}
								<div class="rounded-lg border border-border p-3">
									<div class="mb-2 flex items-center justify-between">
										<span class="text-sm font-medium text-muted-foreground">Row {i + 1}</span>
										<button
											type="button"
											class="rounded p-1 text-muted-foreground transition-colors hover:text-destructive"
											onclick={() => removeRow(i)}
											title="Delete row"
										>
											<Trash2 class="size-3.5" />
										</button>
									</div>
									<div class="grid grid-cols-2 gap-3">
										{#each fields as field (field.name)}
											{@const spec = specFromPortField(field)}
											<div class="space-y-1">
												<span class="text-sm text-muted-foreground">{field.label}</span>
												{#if isFileField(field)}
													{@const val = row[field.name]}
													<div class="space-y-1.5">
														{#if typeof val === 'string' && val !== ''}
															<p class="truncate rounded bg-muted/50 px-1.5 py-0.5 font-mono text-xs">{displayCell(field, val)}</p>
														{/if}
														<div class="flex items-center gap-1.5">
															<FileDropZone.Root
																accept={field.accept ?? undefined}
																maxFiles={1}
																onUpload={(files) => handleFileUpload(i, field.name, files)}
															>
																<FileDropZone.Trigger>
																	<span class="inline-flex items-center gap-1 text-sm"><Upload class="size-3.5" /> Upload</span>
																</FileDropZone.Trigger>
															</FileDropZone.Root>
															<Button variant="outline" size="sm" class="h-7 gap-1 px-2 text-sm" onclick={() => openCatalog(i, field.name)}>
																<FolderOpen class="size-3.5" /> Catalog
															</Button>
														</div>
													</div>
												{:else}
													<FieldWidget
														spec={spec}
														value={row[field.name]}
														booleanWidget="select"
														onchange={(v) => setCell(i, field.name, v)}
													/>
												{/if}
											</div>
										{/each}
									</div>
								</div>
							{/each}
						</div>
					{/if}
				</div>
			{/if}
		</div>

		<div class="flex items-center justify-end gap-2 border-t border-border px-5 py-4">
			<Button variant="ghost" size="sm" onclick={() => (open = false)} disabled={saving}>Cancel</Button>
			<Button size="sm" onclick={save} disabled={saving || !type} data-testid="asset-records-save">
				{saving ? 'Saving…' : 'Save records'}
			</Button>
		</div>
	</SheetContent>
</Sheet.Root>

<CatalogFilePicker bind:open={catalogOpen} onpick={onCatalogPick} />
