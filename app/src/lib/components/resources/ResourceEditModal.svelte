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
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import X from '@lucide/svelte/icons/x';
	import {
		createResource,
		getResource,
		listResourceTypes,
		rotateResource,
		updateResource,
		type ResourceTypeInfo
	} from '$lib/api/resources';

	type Props = {
		open: boolean;
		/** Resource id when editing, `null` when creating. */
		resource_id: string | null;
		/** Optional pre-loaded type list to skip a round-trip when the parent
		 *  already fetched it. */
		types?: ResourceTypeInfo[];
		workspace_id?: string;
		onsaved: () => void;
	};

	let { open = $bindable(), resource_id, types: typesProp = [], workspace_id, onsaved }: Props =
		$props();

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

	// Resolve descriptor from the (lazily-loaded) types list.
	const descriptor = $derived<ResourceTypeInfo | null>(
		types.find((t) => t.name === selectedType) ?? null
	);

	// JSON Schema property entries — derived from `descriptor.schema`. Picks
	// the type, enum, description per field so the input render-decision
	// happens once.
	type FieldSpec = {
		name: string;
		label: string;
		jsonType: 'string' | 'integer' | 'number' | 'boolean' | 'unknown';
		isSecret: boolean;
		isRequired: boolean;
		enumOptions: string[] | null;
		description: string | null;
	};

	const fieldSpecs = $derived.by<FieldSpec[]>(() => {
		if (!descriptor) return [];
		const schema = (descriptor.schema ?? {}) as Record<string, unknown>;
		const props = (schema.properties ?? {}) as Record<string, Record<string, unknown>>;
		const required = (schema.required ?? []) as string[];
		const order = [...descriptor.public_fields, ...descriptor.secret_fields];
		const out: FieldSpec[] = [];
		for (const name of order) {
			const p = props[name] ?? {};
			let jsonType: FieldSpec['jsonType'] = 'unknown';
			const t = p.type;
			if (t === 'string') jsonType = 'string';
			else if (t === 'integer') jsonType = 'integer';
			else if (t === 'number') jsonType = 'number';
			else if (t === 'boolean') jsonType = 'boolean';
			else if (Array.isArray(t)) {
				// `["string","null"]` — pick the non-null half.
				const non = t.find((x) => x !== 'null');
				if (non === 'string') jsonType = 'string';
				else if (non === 'integer') jsonType = 'integer';
				else if (non === 'number') jsonType = 'number';
				else if (non === 'boolean') jsonType = 'boolean';
			}
			out.push({
				name,
				label: name,
				jsonType,
				isSecret: descriptor.secret_fields.includes(name),
				isRequired: required.includes(name),
				enumOptions: Array.isArray(p.enum) ? (p.enum as string[]) : null,
				description: typeof p.description === 'string' ? p.description : null
			});
		}
		return out;
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
				} catch (e) {
					error = e instanceof Error ? e.message : 'Failed to load resource';
				} finally {
					loading = false;
				}
			} else {
				mode = 'create';
				selectedType = '';
				path = '';
				displayName = '';
				fieldValues = {};
			}
		})();
	});

	function setField(name: string, raw: string) {
		fieldValues = { ...fieldValues, [name]: raw };
	}

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
			return 'Lowercase letter first, then letters / digits / underscores. Used as `<path>.<field>` in Python.';
		}
		return null;
	});

	function buildConfig(includeBlankSecrets: boolean): Record<string, unknown> {
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
				error = 'Enter a path';
				return;
			}
			if (pathError) {
				error = pathError;
				return;
			}
		}
		loading = true;
		error = null;
		try {
			if (mode === 'create') {
				await createResource({
					path,
					resource_type: selectedType,
					display_name: displayName || null,
					config: buildConfig(true),
					workspace_id: workspace_id ?? null
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
				<SheetTitle class="text-lg font-semibold">{title}</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					{mode === 'create'
						? 'Typed credential. Secret fields are written to Vault.'
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
						label="Path"
						description="Snake_case identifier. Referenced in workflow code as `<path>.<field>`."
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
							class="text-sm"
						/>
					</FormField>

					{#if descriptor}
						<div class="space-y-3 rounded-md border border-border/60 p-3">
							<div class="text-sm font-medium text-muted-foreground">
								{descriptor.display_name} configuration
							</div>
							{#each fieldSpecs as f (f.name)}
								<FormField
									label={f.label + (f.isSecret ? ' (secret)' : '') + (f.isRequired ? ' *' : '')}
									description={f.description ?? undefined}
								>
									{#if f.enumOptions}
										<Select.Root
											type="single"
											value={fieldValues[f.name] ?? ''}
											onValueChange={(v) => setField(f.name, v ?? '')}
										>
											<Select.Trigger class="w-full text-sm">
												{fieldValues[f.name] || '— select —'}
											</Select.Trigger>
											<Select.Content>
												{#each f.enumOptions as opt (opt)}
													<Select.Item value={opt} label={opt} />
												{/each}
											</Select.Content>
										</Select.Root>
									{:else if f.jsonType === 'boolean'}
										<Select.Root
											type="single"
											value={fieldValues[f.name] ?? ''}
											onValueChange={(v) => setField(f.name, v ?? '')}
										>
											<Select.Trigger class="w-full text-sm">
												{fieldValues[f.name] || '— select —'}
											</Select.Trigger>
											<Select.Content>
												<Select.Item value="true" label="true" />
												<Select.Item value="false" label="false" />
											</Select.Content>
										</Select.Root>
									{:else if f.jsonType === 'integer' || f.jsonType === 'number'}
										<Input
											type="number"
											value={fieldValues[f.name] ?? ''}
											placeholder={f.isSecret && mode === 'edit'
												? '(leave blank to keep current)'
												: undefined}
											oninput={(e) =>
												setField(f.name, (e.currentTarget as HTMLInputElement).value)}
											class="text-sm"
										/>
									{:else if f.isSecret}
										<Input
											type="password"
											value={fieldValues[f.name] ?? ''}
											placeholder={mode === 'edit'
												? '(leave blank to keep current)'
												: undefined}
											oninput={(e) =>
												setField(f.name, (e.currentTarget as HTMLInputElement).value)}
											class="font-mono text-sm"
											data-testid="resource-modal-secret-{f.name}"
										/>
									{:else}
										<Input
											type="text"
											value={fieldValues[f.name] ?? ''}
											oninput={(e) =>
												setField(f.name, (e.currentTarget as HTMLInputElement).value)}
											class="text-sm"
										/>
									{/if}
								</FormField>
							{/each}
						</div>
					{/if}
				</div>
			{/if}
		</div>

		<div class="flex items-center justify-between gap-2 border-t border-border bg-muted/30 px-5 py-3">
			{#if mode === 'edit' && resource_id}
				<Button variant="ghost" size="sm" onclick={rotateOnly} disabled={loading}>
					Rotate secrets
				</Button>
			{:else}
				<span></span>
			{/if}
			<div class="flex items-center gap-2">
				<Button variant="ghost" size="sm" onclick={() => (open = false)}>Cancel</Button>
				<Button size="sm" onclick={submit} disabled={loading || !selectedType}>
					{loading ? 'Saving…' : mode === 'create' ? 'Create resource' : 'Save changes'}
				</Button>
			</div>
		</div>
	</SheetContent>
</Sheet.Root>
