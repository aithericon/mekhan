<script lang="ts">
	// Schema editor for an asset type (docs/20 §4.1). Authors an ordered list of
	// PortFields (reusing the typed-ports PortFieldEditor row), plus cardinality
	// (object | collection), a flat ident `name`, display name, and virtual
	// folder. On edit the server enforces additive-only schema evolution (§4.3);
	// breaking changes surface as a 422/400 ApiError here.
	import { Sheet, SheetContent, SheetTitle, SheetDescription, SheetClose } from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import X from '@lucide/svelte/icons/x';
	import Plus from '@lucide/svelte/icons/plus';
	import ArrowUp from '@lucide/svelte/icons/arrow-up';
	import ArrowDown from '@lucide/svelte/icons/arrow-down';
	import PortFieldEditor from '$lib/components/editor/panels/property-sections/PortFieldEditor.svelte';
	import {
		createAssetType,
		getAssetType,
		updateAssetType,
		type Cardinality,
		type PortField,
		type ScopeContext
	} from '$lib/api/assets';

	type Props = {
		open: boolean;
		/** Asset-type id when editing, `null` when creating. */
		typeId: string | null;
		scope: ScopeContext;
		onsaved: () => void;
	};

	let { open = $bindable(), typeId, scope, onsaved }: Props = $props();

	let mode = $state<'create' | 'edit'>('create');
	let name = $state('');
	let displayName = $state('');
	let displayPath = $state('');
	let cardinality = $state<Cardinality>('collection');
	let fields = $state<PortField[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let lastLoaded = $state<string | null | undefined>(undefined);

	// Mirror the server-side ref-key grammar (^[a-z][a-z0-9_]*$). The asset type
	// name is the flat identifier referenced from bindings, like a resource path.
	const NAME_PATTERN = /^[a-z][a-z0-9_]*$/;
	const nameError = $derived.by(() => {
		if (mode === 'edit') return null; // name is locked on edit
		if (!name) return null;
		if (!NAME_PATTERN.test(name)) {
			return 'Lowercase letter first, then letters / digits / underscores.';
		}
		return null;
	});

	// Reset/prefill when the sheet opens against a different target.
	$effect(() => {
		if (!open) {
			lastLoaded = undefined;
			return;
		}
		const target = typeId;
		if (lastLoaded === target) return;
		lastLoaded = target;
		void bootstrap(target);
	});

	async function bootstrap(target: string | null) {
		error = null;
		if (target === null) {
			mode = 'create';
			name = '';
			displayName = '';
			displayPath = '';
			cardinality = 'collection';
			fields = [];
			return;
		}
		mode = 'edit';
		loading = true;
		try {
			const detail = await getAssetType(target);
			name = detail.name;
			displayName = detail.display_name;
			displayPath = detail.display_path ?? '';
			cardinality = (detail.cardinality as Cardinality) ?? 'collection';
			fields = [...detail.fields];
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load asset type';
		} finally {
			loading = false;
		}
	}

	function addField() {
		fields = [
			...fields,
			{ name: `field_${fields.length + 1}`, label: `Field ${fields.length + 1}`, kind: 'text', required: false }
		];
	}

	function updateField(i: number, field: PortField) {
		fields = fields.map((f, idx) => (idx === i ? field : f));
	}

	function removeField(i: number) {
		fields = fields.filter((_, idx) => idx !== i);
	}

	function moveField(i: number, dir: -1 | 1) {
		const j = i + dir;
		if (j < 0 || j >= fields.length) return;
		const next = [...fields];
		[next[i], next[j]] = [next[j], next[i]];
		fields = next;
	}

	async function submit() {
		error = null;
		if (mode === 'create') {
			if (!name) {
				error = 'Enter a type name';
				return;
			}
			if (nameError) {
				error = nameError;
				return;
			}
		}
		if (fields.length === 0) {
			error = 'Add at least one field';
			return;
		}
		// Field names must be flat idents and unique.
		const seen = new Set<string>();
		for (const f of fields) {
			if (!NAME_PATTERN.test(f.name)) {
				error = `Field "${f.name || '(unnamed)'}" must be a flat identifier.`;
				return;
			}
			if (seen.has(f.name)) {
				error = `Duplicate field name "${f.name}".`;
				return;
			}
			seen.add(f.name);
		}
		loading = true;
		try {
			if (mode === 'create') {
				await createAssetType({
					name,
					display_name: displayName || name,
					display_path: displayPath || null,
					cardinality,
					fields,
					scope_kind: scope.kind,
					scope_id: scope.kind === 'workspace' ? null : scope.id
				});
			} else if (typeId) {
				await updateAssetType(typeId, {
					display_name: displayName || name,
					display_path: displayPath || null,
					fields
				});
			}
			onsaved();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Save failed';
		} finally {
			loading = false;
		}
	}

	const title = $derived(mode === 'create' ? 'New asset type' : 'Edit asset type');
</script>

<Sheet.Root bind:open>
	<SheetContent class="w-[560px] sm:max-w-[560px]">
		<div class="flex items-center justify-between border-b border-border px-5 py-4">
			<div>
				<SheetTitle class="text-lg font-semibold">{title}</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					{mode === 'create'
						? 'Define a curated content schema. Records are validated against these fields.'
						: 'Schema changes are additive-only — add optional fields or widen; rename/remove/retype is rejected.'}
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

			<div class="space-y-4">
				<FormField label="Type name (ref-key)" for="asset-type-name">
					<Input
						id="asset-type-name"
						value={name}
						placeholder="material"
						disabled={mode === 'edit'}
						class="font-mono text-sm"
						oninput={(e) => (name = (e.currentTarget as HTMLInputElement).value)}
					/>
					{#if nameError}
						<p class="mt-1 text-sm text-destructive">{nameError}</p>
					{/if}
				</FormField>

				<FormField label="Display name" for="asset-type-display">
					<Input
						id="asset-type-display"
						value={displayName}
						placeholder="Material"
						class="text-sm"
						oninput={(e) => (displayName = (e.currentTarget as HTMLInputElement).value)}
					/>
				</FormField>

				<FormField label="Folder (display path)" for="asset-type-folder">
					<Input
						id="asset-type-folder"
						value={displayPath}
						placeholder="materials/metals"
						class="font-mono text-sm"
						oninput={(e) => (displayPath = (e.currentTarget as HTMLInputElement).value)}
					/>
				</FormField>

				<FormField label="Cardinality" for="asset-type-cardinality">
					<Select.Root
						type="single"
						value={cardinality}
						onValueChange={(v) => (cardinality = (v as Cardinality) ?? 'collection')}
						disabled={mode === 'edit'}
					>
						<Select.Trigger class="text-sm" disabled={mode === 'edit'}>
							{cardinality === 'object' ? 'Object (single record)' : 'Collection (many records)'}
						</Select.Trigger>
						<Select.Content>
							<Select.Item value="collection" label="Collection (many records)" />
							<Select.Item value="object" label="Object (single record)" />
						</Select.Content>
					</Select.Root>
				</FormField>

				<div class="space-y-2 pt-2">
					<div class="flex items-center justify-between">
						<span class="text-sm font-medium text-muted-foreground">Fields</span>
						<Button variant="outline" size="sm" class="h-7 gap-1 px-2 text-sm" onclick={addField}>
							<Plus class="size-3.5" />
							Add field
						</Button>
					</div>
					{#if fields.length === 0}
						<p class="rounded-md border border-dashed border-border px-3 py-4 text-center text-sm text-muted-foreground">
							No fields yet. Add the columns of this asset type.
						</p>
					{:else}
						<div class="space-y-2">
							{#each fields as field, i (i)}
								<div class="flex items-start gap-1">
									<div class="flex flex-col gap-0.5 pt-2.5">
										<button
											type="button"
											class="rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground disabled:opacity-30"
											disabled={i === 0}
											onclick={() => moveField(i, -1)}
											title="Move up"
										>
											<ArrowUp class="size-3.5" />
										</button>
										<button
											type="button"
											class="rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground disabled:opacity-30"
											disabled={i === fields.length - 1}
											onclick={() => moveField(i, 1)}
											title="Move down"
										>
											<ArrowDown class="size-3.5" />
										</button>
									</div>
									<div class="min-w-0 flex-1">
										<PortFieldEditor
											{field}
											onchange={(f) => updateField(i, f)}
											onremove={() => removeField(i)}
										/>
									</div>
								</div>
							{/each}
						</div>
					{/if}
				</div>
			</div>
		</div>

		<div class="flex items-center justify-end gap-2 border-t border-border px-5 py-4">
			<Button variant="ghost" size="sm" onclick={() => (open = false)} disabled={loading}>
				Cancel
			</Button>
			<Button size="sm" onclick={submit} disabled={loading} data-testid="asset-type-save">
				{loading ? 'Saving…' : mode === 'create' ? 'Create type' : 'Save changes'}
			</Button>
		</div>
	</SheetContent>
</Sheet.Root>
