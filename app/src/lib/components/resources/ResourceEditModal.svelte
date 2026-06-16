<script lang="ts">
	// Schema-driven create / update modal. Read the type's `schema.properties`
	// from `GET /api/resources/types` and render one field per property,
	// using the schemars output to pick an input type (text/number/select).
	// Secret fields use `<input type="password">`; on update the secret
	// inputs default to "leave blank to keep current" — empty values are
	// omitted from the PUT/rotate body so they stay at their stored value.
	import { Sheet, SheetContent, SheetTitle, SheetDescription, SheetClose } from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import SchemaFields, {
		deriveFieldSpecs,
		discriminatorOf,
		type FieldSpec as SchemaFieldSpec
	} from './SchemaFields.svelte';
	import X from '@lucide/svelte/icons/x';
	import {
		createResource,
		getResource,
		listResourceTypes,
		rotateResource,
		updateResource,
		moveResource,
		type ResourceTypeInfo
	} from '$lib/api/resources';
	import PlacementFields from '$lib/components/iam/PlacementFields.svelte';
	import MoveLocationField from '$lib/components/iam/MoveLocationField.svelte';
	import { scopeToParam, type ScopeContext } from '$lib/api/assets';
	import { canMutateResource } from '$lib/api/resource-tier';
	import { auth } from '$lib/auth/store.svelte';
	import { Badge } from '$lib/components/ui/badge';
	import Globe from '@lucide/svelte/icons/globe';

	type Props = {
		open: boolean;
		/** Resource id when editing, `null` when creating. */
		resource_id: string | null;
		/** Optional pre-loaded type list to skip a round-trip when the parent
		 *  already fetched it. */
		types?: ResourceTypeInfo[];
		workspace_id?: string;
		/** Optional resource_type to preselect when CREATING (resource_id null).
		 *  e.g. the Control Plane opens this prefilled to `capacity`. Ignored on
		 *  edit (the existing resource's type wins). */
		prefillType?: string;
		/** When creating from a folder's Resources tab, default the placement to
		 *  that folder so the new resource lands where the user is browsing. */
		defaultFolderId?: string;
		onsaved: () => void;
		/** Refresh the list after a scope move without closing. Defaults to `onsaved`. */
		onmoved?: () => void;
	};

	let {
		open = $bindable(),
		resource_id,
		types: typesProp = [],
		workspace_id,
		prefillType,
		defaultFolderId,
		onsaved,
		onmoved
	}: Props = $props();

	// Locally-mutable copy: when the modal opens with no parent-provided
	// types list, we lazy-load via `listResourceTypes()` into this slot.
	// Initialized in the open-effect below (not via `$state(typesProp)`,
	// which would warn about only capturing the initial prop value).
	let types = $state<ResourceTypeInfo[]>([]);
	let selectedType = $state<string>('');
	let path = $state<string>('');
	let displayName = $state<string>('');
	let fieldValues = $state<Record<string, string>>({});
	let loading = $state(false);
	let error = $state<string | null>(null);
	let mode = $state<'create' | 'edit'>('create');

	// Placement + privacy (create only). Folder/template scope makes the resource
	// non-workspace-wide; `restricted` drops the workspace-role floor (private).
	let scope = $state<ScopeContext>({ kind: 'workspace' });
	let restricted = $state(false);
	// Create-as-platform toggle — platform admins only. When on, the resource is
	// created at the global platform tier (`scope_kind: 'platform'`), bypassing
	// the workspace/folder placement entirely.
	let createAsPlatform = $state(false);

	// The resource's owner scope (edit mode), for the move control. Seeded from
	// the loaded detail and updated optimistically on a successful move.
	let editScope = $state<ScopeContext>({ kind: 'workspace' });
	// Loaded detail's tier signals (edit mode): the precise `scope_kind` and the
	// caller's effective role, for the platform badge + read-only gating.
	let editScopeKind = $state<string>('workspace');
	let editEffectiveRole = $state<string | null>(null);

	const isPlatformAdmin = $derived(auth.isPlatformAdmin);
	// Edit mode: is the loaded resource on the platform tier?
	const editIsPlatform = $derived(mode === 'edit' && editScopeKind === 'platform');
	// Read-only when the caller can't mutate the loaded resource (viewer role —
	// which folds in a non-admin's view of a platform resource). View/run stay
	// intact; edit/rotate/move/save affordances are hidden or disabled.
	const readOnly = $derived(
		mode === 'edit' && !canMutateResource({ my_effective_role: editEffectiveRole })
	);

	async function moveTo(next: ScopeContext) {
		if (!resource_id) return;
		await moveResource(resource_id, scopeToParam(next) ?? 'workspace');
		editScope = next;
		(onmoved ?? onsaved)();
	}

	// Resolve descriptor from the (lazily-loaded) types list.
	const descriptor = $derived<ResourceTypeInfo | null>(
		types.find((t) => t.name === selectedType) ?? null
	);

	// JSON Schema property entries — derived from `descriptor.schema` via the
	// shared `deriveFieldSpecs` (same logic SchemaForm renders with). The
	// field order is the resource type's public-then-secret listing.
	// Discriminator field (e.g. a datacenter's `scheduler_flavor`) for a
	// `oneOf`-discriminated resource schema; `null` for a plain object schema.
	const discriminator = $derived(
		descriptor ? discriminatorOf(descriptor.schema as Record<string, unknown>) : null
	);

	const fieldSpecs = $derived.by<SchemaFieldSpec[]>(() => {
		if (!descriptor) return [];
		// Pass the CURRENT discriminator value so a discriminated schema renders
		// the flavor select + only that flavor's fields (reactively re-derives
		// when the user switches flavor).
		return deriveFieldSpecs(
			(descriptor.schema ?? {}) as Record<string, unknown>,
			descriptor.secret_fields,
			[...descriptor.public_fields, ...descriptor.secret_fields],
			discriminator ? fieldValues[discriminator] : undefined
		);
	});

	// Bootstrap: load types if not provided, and pre-fill when editing.
	$effect(() => {
		if (!open) return;
		void resource_id;
		error = null;
		(async () => {
			// Prefer parent-provided list (current snapshot) — only fall back
			// to a fetch when both the local cache and the prop are empty.
			if (types.length === 0 && typesProp.length > 0) {
				types = typesProp;
			}
			if (types.length === 0) {
				try {
					types = await listResourceTypes();
				} catch (e) {
					error = e instanceof Error ? e.message : 'Failed to load types';
					return;
				}
			}
			if (resource_id) {
				mode = 'edit';
				loading = true;
				try {
					const detail = await getResource(resource_id);
					selectedType = detail.resource_type;
					path = detail.path;
					displayName = detail.display_name;
					editScopeKind = detail.scope_kind;
					editEffectiveRole = detail.my_effective_role ?? null;
					editScope =
						detail.scope_kind === 'workspace' || detail.scope_kind === 'platform'
							? { kind: 'workspace' }
							: {
									kind: detail.scope_kind as 'folder' | 'template',
									id: detail.scope_id ?? ''
								};
					const values: Record<string, string> = {};
					const config = (detail.public_config ?? {}) as Record<string, unknown>;
					for (const [k, v] of Object.entries(config)) {
						if (v === null || v === undefined) values[k] = '';
						else values[k] = String(v);
					}
					// Secret fields stay empty — the modal won't echo back any
					// secret values (they never leave Vault), and the empty
					// string is the "leave unchanged" signal on submit.
					for (const f of detail.redacted_secret_fields) {
						values[f] = '';
					}
					fieldValues = values;
					// For kv resources, surface the existing keys as editable
					// rows with empty values (treated as "leave unchanged" on
					// submit just like typed secret fields).
					const dyn = types.find((t) => t.name === detail.resource_type)?.dynamic_fields;
					if (dyn) {
						seedKvPairsFromDetail(config, detail.redacted_secret_fields);
					}
				} catch (e) {
					error = e instanceof Error ? e.message : 'Failed to load resource';
				} finally {
					loading = false;
				}
			} else {
				mode = 'create';
				// Preselect the prefill type when it's a known resource type;
				// otherwise leave the picker on "choose a type".
				selectedType = prefillType && types.some((t) => t.name === prefillType) ? prefillType : '';
				path = '';
				displayName = '';
				fieldValues = {};
				kvPairs = [];
				scope = defaultFolderId ? { kind: 'folder', id: defaultFolderId } : { kind: 'workspace' };
				restricted = false;
				createAsPlatform = false;
				editScopeKind = 'workspace';
				editEffectiveRole = null;
			}
		})();
	});

	// --- kv (dynamic-fields) editor ----------------------------------------
	// For `kv`-style resources the field set is user-defined. Track an
	// ordered list of `{ key, value }` pairs so the user can add / rename /
	// remove keys; submit converts them into the config object the backend
	// expects (`{ key1: val1, ... }`).
	type KvPair = { key: string; value: string; isNew: boolean };
	let kvPairs = $state<KvPair[]>([]);

	const isDynamic = $derived(descriptor?.dynamic_fields ?? false);

	function seedKvPairsFromDetail(
		publicConfig: Record<string, unknown> | undefined,
		secretFields: string[]
	) {
		const keys: string[] = Array.isArray(publicConfig?.__kv_keys)
			? (publicConfig.__kv_keys as string[]).filter((k) => typeof k === 'string')
			: [...secretFields];
		kvPairs = keys.map((k) => ({ key: k, value: '', isNew: false }));
	}

	function addKvPair() {
		kvPairs = [...kvPairs, { key: '', value: '', isNew: true }];
	}

	function removeKvPair(index: number) {
		kvPairs = kvPairs.filter((_, i) => i !== index);
	}

	function updateKvKey(index: number, key: string) {
		kvPairs = kvPairs.map((p, i) => (i === index ? { ...p, key } : p));
	}

	function updateKvValue(index: number, value: string) {
		kvPairs = kvPairs.map((p, i) => (i === index ? { ...p, value } : p));
	}

	// Same identifier grammar as `<path>` (backend KV_KEY_REGEX). Surfaced
	// inline so the user sees the constraint before the 400 round-trip.
	const KV_KEY_PATTERN = /^[a-z][a-z0-9_]*$/;
	const kvErrors = $derived.by(() => {
		if (!isDynamic) return [] as string[];
		const errs: string[] = [];
		const seen = new Set<string>();
		for (const [i, p] of kvPairs.entries()) {
			const k = p.key.trim();
			if (!k) {
				errs.push(`Row ${i + 1}: key is required.`);
				continue;
			}
			if (!KV_KEY_PATTERN.test(k)) {
				errs.push(`Row ${i + 1}: "${k}" is not a valid identifier.`);
			}
			if (seen.has(k)) errs.push(`Row ${i + 1}: duplicate key "${k}".`);
			seen.add(k);
		}
		if (mode === 'create' && kvPairs.length === 0) {
			errs.push('At least one key is required.');
		}
		return errs;
	});

	// Mirror the server-side regex (`service/src/handlers/resources.rs`).
	// Workflow code references the resource as `<path>.<field>`, so the
	// path itself must be a valid Python identifier — same shape the
	// backend will accept. Surfacing the error here avoids a 400 round-
	// trip on every typo.
	const PATH_PATTERN = /^[a-z][a-z0-9_]*$/;
	const pathError = $derived.by(() => {
		if (mode === 'edit') return null; // path is locked on edit
		if (!path) return null; // empty handled at submit
		if (!PATH_PATTERN.test(path)) {
			return 'Lowercase letter first, then letters / digits / underscores. Used as `<key>.<field>` in Python.';
		}
		return null;
	});

	function buildConfig(includeBlankSecrets: boolean): Record<string, unknown> {
		// kv path: every row contributes a `<key>: <value>` entry. Edit
		// mode skips blank values for existing keys (treated as "leave
		// unchanged" — same shape contract as typed secret fields). New
		// keys must supply a value; the server fails the create if any
		// value is missing.
		if (isDynamic) {
			const out: Record<string, unknown> = {};
			for (const p of kvPairs) {
				const key = p.key.trim();
				if (!key) continue;
				const value = p.value;
				if (!includeBlankSecrets && !p.isNew && value === '') continue;
				out[key] = value;
			}
			return out;
		}
		const out: Record<string, unknown> = {};
		for (const spec of fieldSpecs) {
			const raw = fieldValues[spec.name] ?? '';
			// Edit mode: skip empty secret inputs so the existing Vault value
			// is preserved.
			if (!includeBlankSecrets && spec.isSecret && raw === '') continue;
			if (raw === '' && !spec.isRequired) continue;
			if (spec.jsonType === 'integer') {
				const n = parseInt(raw, 10);
				out[spec.name] = Number.isFinite(n) ? n : 0;
			} else if (spec.jsonType === 'number') {
				const n = parseFloat(raw);
				out[spec.name] = Number.isFinite(n) ? n : 0;
			} else if (spec.jsonType === 'boolean') {
				out[spec.name] = raw === 'true';
			} else {
				out[spec.name] = raw;
			}
		}
		return out;
	}

	async function submit() {
		if (!selectedType) {
			error = 'Choose a resource type';
			return;
		}
		if (mode === 'create') {
			if (!path) {
				error = 'Enter a key';
				return;
			}
			if (pathError) {
				error = pathError;
				return;
			}
		}
		if (isDynamic && kvErrors.length > 0) {
			error = kvErrors[0];
			return;
		}
		loading = true;
		error = null;
		try {
			if (mode === 'create') {
				// Platform tier (admins only) is a global scope above any workspace:
				// it carries no folder/template placement and is never `restricted`.
				const platform = isPlatformAdmin && createAsPlatform;
				await createResource({
					path,
					resource_type: selectedType,
					display_name: displayName || null,
					config: buildConfig(true),
					workspace_id: workspace_id ?? null,
					scope_kind: platform ? 'platform' : scope.kind,
					scope_id: platform || scope.kind === 'workspace' ? null : scope.id,
					restricted: platform ? false : restricted
				});
			} else if (resource_id) {
				// Edit: name-only updates don't carry config; if any non-empty
				// secret value is set we treat that as a config rotation via
				// the PUT path (which bumps version).
				const cfg = buildConfig(false);
				const cfgChanged = Object.keys(cfg).length > 0;
				if (cfgChanged) {
					await updateResource(resource_id, {
						display_name: displayName || null,
						config: cfg
					});
				} else if (displayName) {
					await updateResource(resource_id, {
						display_name: displayName,
						config: null
					});
				}
			}
			onsaved();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Save failed';
		} finally {
			loading = false;
		}
	}

	async function rotateOnly() {
		// Explicit rotate button — exercises POST /rotate so the audit verb
		// records `rotate` rather than `update`. Requires every secret to be
		// re-supplied (the rotate endpoint validates required fields).
		if (!resource_id) return;
		loading = true;
		error = null;
		try {
			await rotateResource(resource_id, { config: buildConfig(true) });
			onsaved();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Rotate failed';
		} finally {
			loading = false;
		}
	}

	const title = $derived(mode === 'create' ? 'New resource' : 'Edit resource');
</script>

<Sheet.Root bind:open>
	<SheetContent class="w-[520px] sm:max-w-[520px]">
		<div class="flex items-center justify-between border-b border-border px-5 py-4">
			<div>
				<div class="flex items-center gap-2">
					<SheetTitle class="text-lg font-semibold">{title}</SheetTitle>
					{#if editIsPlatform}
						<Badge
							class="gap-1 bg-sky-100 text-sky-800"
							variant="secondary"
							title="Platform tier — shared across all workspaces, managed by platform admins"
							data-testid="resource-modal-platform-badge"
						>
							<Globe class="size-3" /> Platform
						</Badge>
					{/if}
				</div>
				<SheetDescription class="text-sm text-muted-foreground">
					{mode === 'create'
						? 'Typed credential. Secret fields are written to Vault.'
						: readOnly
							? 'Read-only — you can view this resource but not change it.'
							: 'Public fields can be edited in place. Provide a secret value to rotate it; leave blank to keep the current value.'}
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

			{#if loading && fieldSpecs.length === 0}
				<p class="text-sm text-muted-foreground">Loading…</p>
			{:else}
				<div class="space-y-4">
					<FormField label="Type">
						<Select.Root
							type="single"
							value={selectedType}
							onValueChange={(v) => (selectedType = v ?? '')}
							disabled={mode === 'edit'}
						>
							<Select.Trigger class="w-full" data-testid="resource-modal-type">
								{descriptor?.display_name ?? selectedType ?? '— select a type —'}
							</Select.Trigger>
							<Select.Content>
								{#each types as t (t.name)}
									<Select.Item value={t.name} label={t.display_name} />
								{/each}
							</Select.Content>
						</Select.Root>
					</FormField>

					<FormField
						label="Key"
						description="Snake_case identifier — the resource's reference key, not a folder. Referenced in workflow code as `<key>.<field>`."
					>
						<Input
							type="text"
							value={path}
							placeholder="local_pg"
							oninput={(e) => (path = (e.currentTarget as HTMLInputElement).value)}
							disabled={mode === 'edit'}
							aria-invalid={pathError ? 'true' : undefined}
							class="font-mono text-sm"
							data-testid="resource-modal-path"
						/>
						{#if pathError}
							<p class="mt-1 text-sm text-destructive" data-testid="resource-modal-path-error">
								{pathError}
							</p>
						{/if}
					</FormField>

					<FormField label="Display name">
						<Input
							type="text"
							value={displayName}
							placeholder={path || 'Optional label'}
							oninput={(e) => (displayName = (e.currentTarget as HTMLInputElement).value)}
							disabled={readOnly}
							class="text-sm"
						/>
					</FormField>

					{#if mode === 'create'}
						{#if isPlatformAdmin}
							<label
								class="flex items-start gap-2.5 rounded-md border border-sky-200 bg-sky-50/60 p-3 text-sm"
								data-testid="resource-modal-platform-toggle"
							>
								<input
									type="checkbox"
									checked={createAsPlatform}
									onchange={(e) =>
										(createAsPlatform = (e.currentTarget as HTMLInputElement).checked)}
									class="mt-0.5 size-4"
								/>
								<span>
									<span class="flex items-center gap-1.5 font-medium text-foreground">
										<Globe class="size-3.5" /> Platform (shared)
									</span>
									<span class="text-muted-foreground">
										Create at the global platform tier — visible read-only to every
										workspace, managed by platform admins. Overrides the location below.
									</span>
								</span>
							</label>
						{/if}
						{#if !createAsPlatform}
							<PlacementFields bind:scope bind:restricted testidPrefix="resource-modal" />
						{/if}
					{:else if !readOnly && !editIsPlatform}
						<MoveLocationField scope={editScope} onMove={moveTo} testid="resource-move" />
					{/if}

					<fieldset disabled={readOnly} class="contents">
					{#if descriptor && isDynamic}
						<div class="space-y-3 rounded-md border border-border/60 p-3">
							<div class="flex items-center justify-between">
								<div class="text-sm font-medium text-muted-foreground">
									Key/Value pairs (all values are secrets)
								</div>
								<Button variant="outline" size="sm" onclick={addKvPair}>
									Add key
								</Button>
							</div>
							{#if kvPairs.length === 0}
								<p class="text-sm text-muted-foreground italic">
									No keys yet. Add at least one before saving.
								</p>
							{/if}
							{#each kvPairs as p, i (i)}
								<div class="flex items-start gap-2">
									<Input
										type="text"
										value={p.key}
										placeholder="api_key"
										oninput={(e) =>
											updateKvKey(i, (e.currentTarget as HTMLInputElement).value)}
										class="font-mono text-sm flex-1"
										disabled={mode === 'edit' && !p.isNew}
										data-testid="resource-modal-kv-key-{i}"
									/>
									<Input
										type="password"
										value={p.value}
										placeholder={mode === 'edit' && !p.isNew
											? '(leave blank to keep current)'
											: 'secret value'}
										oninput={(e) =>
											updateKvValue(i, (e.currentTarget as HTMLInputElement).value)}
										class="font-mono text-sm flex-1"
										data-testid="resource-modal-kv-value-{i}"
									/>
									<Button
										variant="ghost"
										size="sm"
										onclick={() => removeKvPair(i)}
										aria-label="Remove key"
									>
										✕
									</Button>
								</div>
							{/each}
							{#if kvErrors.length > 0}
								<ul class="text-sm text-destructive list-disc pl-5">
									{#each kvErrors as e (e)}
										<li>{e}</li>
									{/each}
								</ul>
							{/if}
							<p class="text-sm text-muted-foreground">
								Key names must be snake_case identifiers — they're referenced in
								workflow code as <code>{path || '&lt;path&gt;'}.&lt;key&gt;</code>.
							</p>
						</div>
					{:else if descriptor}
						<SchemaFields
							{descriptor}
							bind:fieldValues
							secretPlaceholder={mode === 'edit'
								? '(leave blank to keep current)'
								: undefined}
						/>
					{/if}
					</fieldset>
				</div>
			{/if}
		</div>

		<div class="flex items-center justify-between gap-2 border-t border-border bg-muted/30 px-5 py-3">
			{#if mode === 'edit' && resource_id && !readOnly}
				<Button variant="ghost" size="sm" onclick={rotateOnly} disabled={loading}>
					Rotate secrets
				</Button>
			{:else}
				<span></span>
			{/if}
			<div class="flex items-center gap-2">
				<Button variant="ghost" size="sm" onclick={() => (open = false)}>
					{readOnly ? 'Close' : 'Cancel'}
				</Button>
				{#if !readOnly}
					<Button size="sm" onclick={submit} disabled={loading || !selectedType}>
						{loading ? 'Saving…' : mode === 'create' ? 'Create resource' : 'Save changes'}
					</Button>
				{/if}
			</div>
		</div>
	</SheetContent>
</Sheet.Root>
