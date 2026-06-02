<script lang="ts">
	// CSV importer for an asset's records (docs/20 §4.2). Pick a CSV, preview the
	// parsed rows mapped onto the type's fields, then import. The server does the
	// authoritative parse + per-cell coercion + validation; this preview is a
	// client-side best-effort so the author sees the column→field mapping before
	// committing. Headerless CSVs map positionally to the type's field order.
	import { Sheet, SheetContent, SheetTitle, SheetDescription, SheetClose } from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import * as FileDropZone from '$lib/components/ui/file-drop-zone';
	import X from '@lucide/svelte/icons/x';
	import FileSpreadsheet from '@lucide/svelte/icons/file-spreadsheet';
	import {
		getAssetType,
		importAssetCsv,
		type AssetSummary,
		type AssetTypeDetail,
		type PortField
	} from '$lib/api/assets';

	type Props = {
		open: boolean;
		/** The asset to import into. */
		asset: AssetSummary | null;
		onsaved: () => void;
	};

	let { open = $bindable(), asset, onsaved }: Props = $props();

	let type = $state<AssetTypeDetail | null>(null);
	let file = $state<File | null>(null);
	let hasHeader = $state(true);
	let append = $state(false);
	let previewRows = $state<string[][]>([]);
	let previewHeaders = $state<string[]>([]);
	let loading = $state(false);
	let importing = $state(false);
	let error = $state<string | null>(null);
	let loadedFor = $state<string | null>(null);

	const fields = $derived<PortField[]>(type?.fields ?? []);

	$effect(() => {
		if (!open || !asset) {
			loadedFor = null;
			return;
		}
		if (loadedFor === asset.id) return;
		loadedFor = asset.id;
		file = null;
		previewRows = [];
		previewHeaders = [];
		error = null;
		void loadType(asset);
	});

	async function loadType(a: AssetSummary) {
		loading = true;
		try {
			type = await getAssetType(a.type_id);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load asset type';
		} finally {
			loading = false;
		}
	}

	// Minimal client-side CSV parse for the PREVIEW only (handles quoted cells +
	// embedded commas/newlines). The server is the authoritative parser.
	function parseCsv(text: string): string[][] {
		const rows: string[][] = [];
		let row: string[] = [];
		let cell = '';
		let inQuotes = false;
		for (let i = 0; i < text.length; i++) {
			const c = text[i];
			if (inQuotes) {
				if (c === '"') {
					if (text[i + 1] === '"') {
						cell += '"';
						i++;
					} else {
						inQuotes = false;
					}
				} else {
					cell += c;
				}
			} else if (c === '"') {
				inQuotes = true;
			} else if (c === ',') {
				row.push(cell);
				cell = '';
			} else if (c === '\n' || c === '\r') {
				if (c === '\r' && text[i + 1] === '\n') i++;
				row.push(cell);
				cell = '';
				if (row.length > 1 || row[0] !== '') rows.push(row);
				row = [];
			} else {
				cell += c;
			}
		}
		if (cell !== '' || row.length > 0) {
			row.push(cell);
			if (row.length > 1 || row[0] !== '') rows.push(row);
		}
		return rows;
	}

	async function onFile(files: File[]) {
		if (files.length === 0) return;
		file = files[0];
		error = null;
		try {
			const text = await file.text();
			const parsed = parseCsv(text);
			rebuildPreview(parsed);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to read CSV';
		}
	}

	// Re-derive the preview whenever the file or the header toggle changes.
	let rawParsed = $state<string[][]>([]);
	function rebuildPreview(parsed: string[][]) {
		rawParsed = parsed;
		applyPreview();
	}
	function applyPreview() {
		const parsed = rawParsed;
		if (parsed.length === 0) {
			previewHeaders = [];
			previewRows = [];
			return;
		}
		if (hasHeader) {
			previewHeaders = parsed[0];
			previewRows = parsed.slice(1, 11);
		} else {
			previewHeaders = fields.map((f) => f.name);
			previewRows = parsed.slice(0, 10);
		}
	}

	$effect(() => {
		void hasHeader;
		void fields;
		applyPreview();
	});

	async function runImport() {
		if (!asset || !file) return;
		importing = true;
		error = null;
		try {
			await importAssetCsv(asset.id, file, { hasHeader, append });
			onsaved();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Import failed';
		} finally {
			importing = false;
		}
	}

	// Which field a preview column maps to (header-name match, or positional).
	function mappedField(header: string, colIdx: number): PortField | undefined {
		if (hasHeader) return fields.find((f) => f.name === header);
		return fields[colIdx];
	}
</script>

<Sheet.Root bind:open>
	<SheetContent class="w-[680px] sm:max-w-[680px]">
		<div class="flex items-center justify-between border-b border-border px-5 py-4">
			<div class="min-w-0">
				<SheetTitle class="text-lg font-semibold">Import CSV</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					{#if asset}
						Into <span class="font-mono">{asset.ref_key}</span> · columns map to the type's fields.
					{:else}
						Map columns onto the type's fields.
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

			{#if loading}
				<p class="py-12 text-center text-sm text-muted-foreground">Loading…</p>
			{:else}
				<div class="space-y-4">
					<FileDropZone.Root accept=".csv,text/csv" maxFiles={1} onUpload={onFile}>
						<FileDropZone.Trigger>
							<span class="inline-flex items-center gap-2 text-sm">
								<FileSpreadsheet class="size-4" />
								{file ? file.name : 'Choose a CSV file'}
							</span>
						</FileDropZone.Trigger>
					</FileDropZone.Root>

					<div class="flex flex-wrap items-center gap-6">
						<label class="flex items-center gap-2">
							<Checkbox
								checked={hasHeader}
								onCheckedChange={(v) => (hasHeader = v === true)}
							/>
							<span class="text-sm text-muted-foreground">First row is a header</span>
						</label>
						<label class="flex items-center gap-2">
							<Checkbox checked={append} onCheckedChange={(v) => (append = v === true)} />
							<span class="text-sm text-muted-foreground">Append to existing records</span>
						</label>
					</div>

					{#if previewRows.length > 0}
						<div class="space-y-2">
							<span class="text-sm font-medium text-muted-foreground">
								Preview (first {previewRows.length} rows)
							</span>
							<div class="overflow-x-auto rounded-lg border border-border">
								<table class="w-full text-sm">
									<thead class="bg-muted/40">
										<tr>
											{#each previewHeaders as header, c (c)}
												{@const mapped = mappedField(header, c)}
												<th class="px-3 py-2 text-left font-medium">
													<div class="flex flex-col">
														<span class="font-mono text-xs">{header || `col ${c + 1}`}</span>
														{#if mapped}
															<span class="text-xs text-primary">→ {mapped.name}</span>
														{:else}
															<span class="text-xs text-muted-foreground italic">ignored</span>
														{/if}
													</div>
												</th>
											{/each}
										</tr>
									</thead>
									<tbody>
										{#each previewRows as r, ri (ri)}
											<tr class="border-t border-border/60">
												{#each previewHeaders as _, c (c)}
													<td class="px-3 py-1.5 text-muted-foreground">{r[c] ?? ''}</td>
												{/each}
											</tr>
										{/each}
									</tbody>
								</table>
							</div>
						</div>
					{/if}
				</div>
			{/if}
		</div>

		<div class="flex items-center justify-end gap-2 border-t border-border px-5 py-4">
			<Button variant="ghost" size="sm" onclick={() => (open = false)} disabled={importing}>
				Cancel
			</Button>
			<Button size="sm" onclick={runImport} disabled={importing || !file} data-testid="asset-csv-import">
				{importing ? 'Importing…' : 'Import'}
			</Button>
		</div>
	</SheetContent>
</Sheet.Root>
