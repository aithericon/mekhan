<script lang="ts" module>
	export type SchemaShape =
		| { kind: 'multi'; title?: string; fields: BuilderField[] }
		| { kind: 'scalar'; type: ScalarType; title?: string; description?: string }
		| { kind: 'raw_only'; reason: string };

	export type ScalarType = 'string' | 'textarea' | 'number' | 'integer' | 'boolean';

	export type BuilderField = {
		name: string;
		type: ScalarType;
		required: boolean;
		description?: string;
	};

	const SCALAR_TYPES: ScalarType[] = ['string', 'textarea', 'number', 'integer', 'boolean'];

	// Detect if a raw JSON schema can be round-tripped through the builder.
	// The builder only handles flat object+properties (with scalar properties)
	// and root scalars. Anything with $ref, anyOf, oneOf, allOf, nested
	// object/array properties, or unknown shapes is left to raw JSON.
	export function detectShape(schema: unknown): SchemaShape {
		if (schema == null || typeof schema !== 'object') {
			return { kind: 'multi', fields: [] };
		}
		const s = schema as Record<string, unknown>;
		if (Object.keys(s).length === 0) {
			return { kind: 'multi', fields: [] };
		}
		const advanced = ['$ref', 'anyOf', 'oneOf', 'allOf', 'not', 'patternProperties'];
		for (const k of advanced) {
			if (k in s) return { kind: 'raw_only', reason: `Uses \`${k}\` — raw JSON only.` };
		}
		const t = s.type as string | undefined;
		const title = typeof s.title === 'string' ? (s.title as string) : undefined;

		if (t === 'object') {
			const props = s.properties as Record<string, unknown> | undefined;
			if (!props) return { kind: 'multi', title, fields: [] };
			const required = Array.isArray(s.required) ? (s.required as string[]) : [];
			const fields: BuilderField[] = [];
			for (const [name, prop] of Object.entries(props)) {
				if (!prop || typeof prop !== 'object') {
					return { kind: 'raw_only', reason: `Property \`${name}\` is not an object.` };
				}
				const p = prop as Record<string, unknown>;
				const propType = p.type as string | undefined;
				if (propType === 'object' || propType === 'array') {
					return {
						kind: 'raw_only',
						reason: `Property \`${name}\` is nested (${propType}) — edit as raw JSON.`
					};
				}
				const inferred = inferScalarType(p);
				if (!inferred) {
					return {
						kind: 'raw_only',
						reason: `Property \`${name}\` has an unsupported type (\`${propType ?? 'none'}\`).`
					};
				}
				fields.push({
					name,
					type: inferred,
					required: required.includes(name),
					description: typeof p.description === 'string' ? (p.description as string) : undefined
				});
			}
			return { kind: 'multi', title, fields };
		}

		if (t === 'string' || t === 'number' || t === 'integer' || t === 'boolean') {
			const inferred = inferScalarType(s) ?? (t as ScalarType);
			return {
				kind: 'scalar',
				type: inferred,
				title,
				description: typeof s.description === 'string' ? (s.description as string) : undefined
			};
		}

		if (t === 'array') {
			return { kind: 'raw_only', reason: 'Root array — edit as raw JSON.' };
		}

		return { kind: 'raw_only', reason: `Unrecognized schema shape (type=\`${t ?? 'none'}\`).` };
	}

	function inferScalarType(s: Record<string, unknown>): ScalarType | null {
		const t = s.type as string | undefined;
		const fmt = s.format as string | undefined;
		if (t === 'string') {
			return fmt === 'textarea' || fmt === 'multi-line' ? 'textarea' : 'string';
		}
		if (t === 'number') return 'number';
		if (t === 'integer') return 'integer';
		if (t === 'boolean') return 'boolean';
		return null;
	}

	export function shapeToSchema(shape: SchemaShape): Record<string, unknown> {
		if (shape.kind === 'multi') {
			const properties: Record<string, unknown> = {};
			const required: string[] = [];
			for (const f of shape.fields) {
				if (!f.name) continue;
				properties[f.name] = scalarPropertySchema(f.type, f.description);
				if (f.required) required.push(f.name);
			}
			const out: Record<string, unknown> = { type: 'object', properties };
			if (shape.title) out.title = shape.title;
			if (required.length > 0) out.required = required;
			out.additionalProperties = false;
			return out;
		}
		if (shape.kind === 'scalar') {
			const out: Record<string, unknown> = scalarPropertySchema(shape.type, shape.description);
			if (shape.title) out.title = shape.title;
			return out;
		}
		return {};
	}

	function scalarPropertySchema(t: ScalarType, description?: string): Record<string, unknown> {
		const out: Record<string, unknown> = {};
		if (t === 'textarea') {
			out.type = 'string';
			out.format = 'textarea';
		} else {
			out.type = t;
		}
		if (description) out.description = description;
		return out;
	}

	export { SCALAR_TYPES };
</script>

<script lang="ts">
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import * as Select from '$lib/components/ui/select';
	import { untrack } from 'svelte';

	type Props = {
		schema: Record<string, unknown>;
		readonly?: boolean;
		onchange: (schema: Record<string, unknown>) => void;
	};

	let { schema, readonly = false, onchange }: Props = $props();

	const shape = $derived(detectShape(schema));

	const typeLabels: Record<ScalarType, string> = {
		string: 'Text',
		textarea: 'Long text',
		number: 'Number',
		integer: 'Integer',
		boolean: 'Boolean'
	};

	let expandedFields = $state<Set<string>>(new Set());

	function slugifyName(label: string): string {
		return label
			.toLowerCase()
			.replace(/[^a-z0-9]+/g, '_')
			.replace(/^_|_$/g, '');
	}

	function toggleExpanded(name: string) {
		const next = new Set(expandedFields);
		if (next.has(name)) next.delete(name);
		else next.add(name);
		expandedFields = next;
	}

	function emitShape(s: SchemaShape) {
		onchange(shapeToSchema(s));
	}

	function setMode(mode: 'multi' | 'scalar') {
		if (shape.kind === mode) return;
		if (mode === 'multi') {
			emitShape({ kind: 'multi', fields: [] });
		} else {
			emitShape({ kind: 'scalar', type: 'string' });
		}
	}

	function addField() {
		if (shape.kind !== 'multi') return;
		const used = new Set(shape.fields.map((f) => f.name));
		let i = shape.fields.length + 1;
		let name = `field_${i}`;
		while (used.has(name)) {
			i += 1;
			name = `field_${i}`;
		}
		emitShape({
			...shape,
			fields: [...shape.fields, { name, type: 'string', required: false }]
		});
	}

	function updateField(idx: number, next: BuilderField) {
		if (shape.kind !== 'multi') return;
		const fields = [...shape.fields];
		fields[idx] = next;
		emitShape({ ...shape, fields });
	}

	function removeField(idx: number) {
		if (shape.kind !== 'multi') return;
		const fields = shape.fields.filter((_, i) => i !== idx);
		emitShape({ ...shape, fields });
	}

	function setScalarType(t: ScalarType) {
		if (shape.kind !== 'scalar') return;
		emitShape({ ...shape, type: t });
	}

	function setScalarTitle(title: string) {
		if (shape.kind !== 'scalar') return;
		emitShape({ ...shape, title: title || undefined });
	}

	function setScalarDescription(description: string) {
		if (shape.kind !== 'scalar') return;
		emitShape({ ...shape, description: description || undefined });
	}

	function setMultiTitle(title: string) {
		if (shape.kind !== 'multi') return;
		emitShape({ ...shape, title: title || undefined });
	}

	// Track which mode the user picked (defaults to the detected mode).
	// Needed because an empty object schema reads as `multi`-with-no-fields
	// but we want the user to be able to switch to `scalar` even when the
	// stored shape is empty.
	let activeMode = $state<'multi' | 'scalar' | 'raw_only'>(
		shape.kind === 'raw_only' ? 'raw_only' : shape.kind
	);
	$effect(() => {
		const k = shape.kind;
		untrack(() => {
			if (k === 'raw_only') {
				activeMode = 'raw_only';
			} else if (activeMode === 'raw_only') {
				activeMode = k;
			}
		});
	});
</script>

{#if shape.kind === 'raw_only'}
	<p class="text-sm text-muted-foreground" data-testid="builder-raw-only">
		{shape.reason} Switch to Raw JSON to keep editing.
	</p>
{:else}
	<div class="space-y-3">
		<div class="flex gap-1.5" data-testid="builder-mode-toggle">
			<button
				type="button"
				class="flex-1 rounded-md border px-2 py-1 text-sm transition-colors {activeMode === 'multi'
					? 'border-primary bg-primary/5 text-foreground'
					: 'border-border text-muted-foreground hover:bg-accent/30'}"
				disabled={readonly}
				onclick={() => {
					activeMode = 'multi';
					setMode('multi');
				}}
				data-testid="builder-mode-multi"
			>
				Multiple fields
			</button>
			<button
				type="button"
				class="flex-1 rounded-md border px-2 py-1 text-sm transition-colors {activeMode === 'scalar'
					? 'border-primary bg-primary/5 text-foreground'
					: 'border-border text-muted-foreground hover:bg-accent/30'}"
				disabled={readonly}
				onclick={() => {
					activeMode = 'scalar';
					setMode('scalar');
				}}
				data-testid="builder-mode-scalar"
			>
				Single value
			</button>
		</div>

		{#if shape.kind === 'multi'}
			<div class="space-y-1.5">
				<Label class="text-sm text-muted-foreground">Title (optional)</Label>
				<Input
					type="text"
					value={shape.title ?? ''}
					placeholder="e.g. Extraction"
					disabled={readonly}
					oninput={(e) => setMultiTitle((e.currentTarget as HTMLInputElement).value)}
				/>
			</div>

			<div class="space-y-2">
				{#each shape.fields as field, i (i)}
					{@const expanded = expandedFields.has(field.name)}
					<div class="rounded-md border border-border/50 bg-background text-sm">
						<div class="flex items-center gap-2 p-2.5">
							<button
								type="button"
								class="rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
								onclick={() => toggleExpanded(field.name)}
							>
								{#if expanded}
									<ChevronDown class="size-4" />
								{:else}
									<ChevronRight class="size-4" />
								{/if}
							</button>
							<Input
								type="text"
								value={field.name}
								placeholder="field_name"
								disabled={readonly}
								oninput={(e) =>
									updateField(i, {
										...field,
										name: slugifyName((e.currentTarget as HTMLInputElement).value)
									})}
								class="flex-1 font-mono"
								data-testid="builder-field-name-{i}"
							/>
							<div class="w-[120px] shrink-0">
								<Select.Root
									type="single"
									value={field.type}
									onValueChange={(v) => {
										if (v) updateField(i, { ...field, type: v as ScalarType });
									}}
									disabled={readonly}
								>
									<Select.Trigger disabled={readonly} class="h-9 px-2 text-sm">
										{typeLabels[field.type]}
									</Select.Trigger>
									<Select.Content>
										{#each SCALAR_TYPES as t (t)}
											<Select.Item value={t} label={typeLabels[t]} />
										{/each}
									</Select.Content>
								</Select.Root>
							</div>
							<label class="flex items-center gap-1.5">
								<Checkbox
									checked={field.required}
									disabled={readonly}
									onCheckedChange={(v) => updateField(i, { ...field, required: v === true })}
								/>
								<span class="text-sm text-muted-foreground">Required</span>
							</label>
							{#if !readonly}
								<button
									type="button"
									class="rounded p-1 text-muted-foreground transition-colors hover:text-destructive"
									onclick={() => removeField(i)}
									data-testid="builder-field-remove-{i}"
								>
									<Trash2 class="size-4" />
								</button>
							{/if}
						</div>
						{#if expanded}
							<div class="space-y-1.5 border-t border-border/50 p-3">
								<Label class="text-sm text-muted-foreground">Description (optional)</Label>
								<Input
									type="text"
									value={field.description ?? ''}
									placeholder="What this field captures…"
									disabled={readonly}
									oninput={(e) =>
										updateField(i, {
											...field,
											description: (e.currentTarget as HTMLInputElement).value || undefined
										})}
								/>
							</div>
						{/if}
					</div>
				{/each}

				{#if !readonly}
					<button
						type="button"
						class="flex w-full items-center justify-center gap-1.5 rounded-md border border-dashed border-border py-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
						onclick={addField}
						data-testid="builder-add-field"
					>
						<Plus class="size-4" />
						Add field
					</button>
				{/if}
			</div>
		{:else if shape.kind === 'scalar'}
			<div class="space-y-3">
				<div class="space-y-1.5">
					<Label class="text-sm text-muted-foreground">Type</Label>
					<Select.Root
						type="single"
						value={shape.type}
						onValueChange={(v) => {
							if (v) setScalarType(v as ScalarType);
						}}
						disabled={readonly}
					>
						<Select.Trigger disabled={readonly}>
							{typeLabels[shape.type]}
						</Select.Trigger>
						<Select.Content>
							{#each SCALAR_TYPES as t (t)}
								<Select.Item value={t} label={typeLabels[t]} />
							{/each}
						</Select.Content>
					</Select.Root>
				</div>
				<div class="space-y-1.5">
					<Label class="text-sm text-muted-foreground">Title (optional)</Label>
					<Input
						type="text"
						value={shape.title ?? ''}
						placeholder="e.g. Sentiment"
						disabled={readonly}
						oninput={(e) => setScalarTitle((e.currentTarget as HTMLInputElement).value)}
					/>
				</div>
				<div class="space-y-1.5">
					<Label class="text-sm text-muted-foreground">Description (optional)</Label>
					<Input
						type="text"
						value={shape.description ?? ''}
						placeholder="What the model should produce…"
						disabled={readonly}
						oninput={(e) => setScalarDescription((e.currentTarget as HTMLInputElement).value)}
					/>
				</div>
			</div>
		{/if}
	</div>
{/if}
