<script lang="ts" module>
	// Shared schema→widget renderer. Reads a JSON Schema's `properties` and
	// renders one input per property, picking the widget from the property's
	// `type` / `enum` and whether the field is flagged secret. Extracted from
	// `resources/ResourceEditModal.svelte` (its FieldSpec derivation + widget
	// chain) so the resource modal and the editor's simple config panels share
	// one renderer and can't drift.
	//
	// The value model is the native-typed config object (`Record<string,
	// unknown>`): booleans stay booleans, arrays stay arrays. `onchange`
	// receives the full next object. ResourceEditModal adapts its string-based
	// model around this (see that file).

	export type JsonType =
		| 'string'
		| 'integer'
		| 'number'
		| 'boolean'
		| 'array'
		| 'object'
		| 'unknown';

	export type FieldSpec = {
		name: string;
		label: string;
		jsonType: JsonType;
		isSecret: boolean;
		isRequired: boolean;
		enumOptions: string[] | null;
		description: string | null;
		/** For `array`: the item primitive (only `string` arrays get a widget). */
		itemType: JsonType;
		/** For `object`: nested sub-schema (a map-of-strings via
		 *  `additionalProperties`, or a fixed-shape object via `properties`). */
		objectSchema: Record<string, unknown> | null;
		/** JSON-Schema `default` for this property (`undefined` if none). Used as
		 *  the effective value when the bound config has the key absent — keeps
		 *  the widget in sync with the backend's serde default. */
		default: unknown;
	};

	function pickPrimitive(t: unknown): JsonType {
		if (t === 'string') return 'string';
		if (t === 'integer') return 'integer';
		if (t === 'number') return 'number';
		if (t === 'boolean') return 'boolean';
		if (t === 'array') return 'array';
		if (t === 'object') return 'object';
		if (Array.isArray(t)) {
			// `["string","null"]` — pick the non-null half.
			const non = t.find((x) => x !== 'null');
			return pickPrimitive(non);
		}
		return 'unknown';
	}

	/** Build a single `FieldSpec` from a JSON-Schema property node. */
	function specFromProp(
		name: string,
		p: Record<string, unknown>,
		secretFields: string[],
		required: string[]
	): FieldSpec {
		const jsonType = pickPrimitive(p.type);
		let itemType: JsonType = 'unknown';
		if (jsonType === 'array') {
			const items = (p.items ?? {}) as Record<string, unknown>;
			itemType = pickPrimitive(items.type);
		}
		let objectSchema: Record<string, unknown> | null = null;
		if (jsonType === 'object') {
			objectSchema = p as Record<string, unknown>;
		}
		return {
			name,
			label: name,
			jsonType,
			isSecret: secretFields.includes(name),
			isRequired: required.includes(name),
			enumOptions: Array.isArray(p.enum) ? (p.enum as string[]) : null,
			description: typeof p.description === 'string' ? p.description : null,
			itemType,
			objectSchema,
			default: 'default' in p ? p.default : undefined
		};
	}

	type Variant = { props: Record<string, Record<string, unknown>>; required: string[] };

	function variantsOf(schema: Record<string, unknown>): Variant[] | null {
		const oneOf = schema.oneOf ?? schema.anyOf;
		if (!Array.isArray(oneOf) || oneOf.length === 0) return null;
		return oneOf.map((v) => ({
			props: ((v as Record<string, unknown>).properties ?? {}) as Record<
				string,
				Record<string, unknown>
			>,
			required: (((v as Record<string, unknown>).required ?? []) as string[]) ?? []
		}));
	}

	/**
	 * The discriminator field of a `oneOf`/`anyOf` schema, or `null` for a plain
	 * object schema. The discriminator is the property present in EVERY variant
	 * with a single-value `enum` (the serde internally-tagged-enum shape — e.g. a
	 * datacenter's `scheduler_flavor`).
	 */
	export function discriminatorOf(
		schema: Record<string, unknown> | null | undefined
	): string | null {
		if (!schema) return null;
		const variants = variantsOf(schema);
		if (!variants) return null;
		for (const name of Object.keys(variants[0].props)) {
			const constInAll = variants.every((v) => {
				const q = v.props[name];
				return Array.isArray(q?.enum) && (q.enum as unknown[]).length === 1;
			});
			if (constInAll) return name;
		}
		return null;
	}

	/**
	 * Derive an ordered list of field specs from a JSON Schema object.
	 *
	 * Plain object schema: one spec per `properties` entry. `fieldOrder`, when
	 * supplied, fixes the iteration order (the resource modal passes
	 * `[...public_fields, ...secret_fields]`).
	 *
	 * DISCRIMINATED schema (`oneOf`/`anyOf` of variants tagged by a const-enum
	 * discriminator — a serde internally-tagged enum like the datacenter's
	 * slurm/nomad/http): render the discriminator as a `<select>` first, then
	 * ONLY the fields of the variant matching `discriminatorValue` (with that
	 * variant's `required`). Nothing else is shown, so a Slurm datacenter never
	 * surfaces the Nomad/HTTP fields. `discriminatorValue` unset ⇒ just the
	 * select (pick a flavor first).
	 */
	export function deriveFieldSpecs(
		schema: Record<string, unknown> | null | undefined,
		secretFields: string[],
		fieldOrder?: string[],
		discriminatorValue?: string
	): FieldSpec[] {
		if (!schema) return [];

		const disc = discriminatorOf(schema);
		if (disc) {
			const variants = variantsOf(schema)!;
			const options = variants
				.map((v) => (v.props[disc]?.enum as string[] | undefined)?.[0])
				.filter((x): x is string => typeof x === 'string');
			const discSpec: FieldSpec = {
				name: disc,
				label: disc,
				jsonType: 'string',
				isSecret: false,
				isRequired: true,
				enumOptions: options,
				description: typeof schema.title === 'string' ? null : null,
				itemType: 'unknown',
				objectSchema: null,
				default: undefined
			};
			const out: FieldSpec[] = [discSpec];
			const active = variants.find(
				(v) => (v.props[disc]?.enum as string[] | undefined)?.[0] === discriminatorValue
			);
			if (active) {
				// Honour the caller's preferred order, then any remaining variant
				// fields, skipping the discriminator itself (rendered above).
				const present = Object.keys(active.props).filter((n) => n !== disc);
				const ordered = [
					...(fieldOrder ?? []).filter((n) => present.includes(n)),
					...present.filter((n) => !(fieldOrder ?? []).includes(n))
				];
				for (const name of ordered) {
					out.push(specFromProp(name, active.props[name] ?? {}, secretFields, active.required));
				}
			}
			return out;
		}

		const props = (schema.properties ?? {}) as Record<string, Record<string, unknown>>;
		const required = (schema.required ?? []) as string[];
		const order = fieldOrder ?? Object.keys(props);
		const out: FieldSpec[] = [];
		for (const name of order) {
			if (!props[name]) continue;
			out.push(specFromProp(name, props[name] ?? {}, secretFields, required));
		}
		return out;
	}
</script>

<script lang="ts">
	import { Input } from '$lib/components/ui/input';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import StringListEditor from './StringListEditor.svelte';
	import KeyValueEditor from './KeyValueEditor.svelte';
	import Self from './SchemaForm.svelte';

	type Props = {
		/** JSON Schema object (with `properties` / `required`). */
		schema: Record<string, unknown> | null | undefined;
		/** Native-typed config value. */
		value: Record<string, unknown>;
		/** Field names rendered with a masked (password) widget. */
		secretFields?: string[];
		readonly?: boolean;
		/** Fixes field iteration order; defaults to schema property order. */
		fieldOrder?: string[];
		/** Whether to render `boolean` fields as a Checkbox (editor panels) or
		 *  a true/false Select (resource modal's string model). */
		booleanWidget?: 'checkbox' | 'select';
		/** Placeholder shown on secret inputs (e.g. "(leave blank to keep
		 *  current)" in the resource edit modal). */
		secretPlaceholder?: string;
		/** When true, integer/number inputs emit a `number` (editor panels'
		 *  native-typed config); when false they emit the raw string (the
		 *  resource modal's string model, which coerces at submit). */
		coerceNumbers?: boolean;
		/** Receives the full next value object. */
		onchange: (next: Record<string, unknown>) => void;
	};

	let {
		schema,
		value,
		secretFields = [],
		readonly = false,
		fieldOrder,
		booleanWidget = 'checkbox',
		secretPlaceholder,
		coerceNumbers = false,
		onchange
	}: Props = $props();

	// Discriminator field for a `oneOf` schema (e.g. a datacenter's
	// `scheduler_flavor`); `null` for a plain object schema.
	const discriminator = $derived(discriminatorOf(schema));
	// Thread the CURRENT discriminator value so a discriminated schema renders
	// the discriminator select + only the matching variant's fields, and
	// re-derives reactively when the user switches it.
	const fieldSpecs = $derived(
		deriveFieldSpecs(
			schema,
			secretFields,
			fieldOrder,
			discriminator && value[discriminator] != null
				? String(value[discriminator])
				: undefined
		)
	);

	function set(name: string, raw: unknown) {
		onchange({ ...value, [name]: raw });
	}

	function asString(v: unknown): string {
		if (v === null || v === undefined) return '';
		return String(v);
	}

	/**
	 * Effective display value for a field: the bound config value when present,
	 * otherwise the schema's `default` (so a freshly created step shows the
	 * backend's serde default rather than a type-zero). Secret fields never
	 * carry a default — leave them blank. Does not mutate the config: the
	 * default only drives what the widget shows until the user touches it.
	 */
	function effective(f: FieldSpec): unknown {
		const v = value[f.name];
		if (v !== undefined && v !== null) return v;
		if (f.isSecret) return v;
		return f.default;
	}
</script>

{#each fieldSpecs as f (f.name)}
	{#if f.jsonType === 'array' && f.itemType === 'string'}
		<div class="space-y-1.5">
			<span class="text-sm font-medium text-muted-foreground"
				>{f.label}{f.isRequired ? ' *' : ''}</span
			>
			{#if f.description}
				<p class="text-sm text-muted-foreground">{f.description}</p>
			{/if}
			<StringListEditor
				items={(value[f.name] as string[]) ?? []}
				{readonly}
				onchange={(items) => set(f.name, items)}
			/>
		</div>
	{:else if f.jsonType === 'object'}
		<div class="space-y-1.5">
			<span class="text-sm font-medium text-muted-foreground"
				>{f.label}{f.isRequired ? ' *' : ''}</span
			>
			{#if f.description}
				<p class="text-sm text-muted-foreground">{f.description}</p>
			{/if}
			{#if f.objectSchema?.properties}
				<!-- Fixed-shape nested object: render its sub-fields recursively. -->
				<div class="space-y-3 rounded-md border border-border/60 p-3">
					<Self
						schema={f.objectSchema}
						value={(value[f.name] as Record<string, unknown>) ?? {}}
						{readonly}
						{booleanWidget}
						{coerceNumbers}
						onchange={(sub) => set(f.name, sub)}
					/>
				</div>
			{:else}
				<!-- Open map (additionalProperties): key/value editor. -->
				<KeyValueEditor
					entries={(value[f.name] as Record<string, unknown>) ?? {}}
					{readonly}
					onchange={(entries) => set(f.name, entries)}
				/>
			{/if}
		</div>
	{:else}
		<FormField
			label={f.label + (f.isSecret ? ' (secret)' : '') + (f.isRequired ? ' *' : '')}
			description={f.description ?? undefined}
		>
			{#if f.enumOptions}
				<Select.Root
					type="single"
					value={asString(effective(f))}
					onValueChange={(v) => set(f.name, v ?? '')}
					disabled={readonly}
				>
					<Select.Trigger class="w-full text-sm">
						{asString(effective(f)) || '— select —'}
					</Select.Trigger>
					<Select.Content>
						{#each f.enumOptions as opt (opt)}
							<Select.Item value={opt} label={opt} />
						{/each}
					</Select.Content>
				</Select.Root>
			{:else if f.jsonType === 'boolean' && booleanWidget === 'checkbox'}
				<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
					<Checkbox
						checked={(effective(f) as boolean) ?? false}
						disabled={readonly}
						onCheckedChange={(v) => set(f.name, v)}
					/>
					{f.label}
				</label>
			{:else if f.jsonType === 'boolean'}
				<Select.Root
					type="single"
					value={asString(effective(f))}
					onValueChange={(v) => set(f.name, v ?? '')}
					disabled={readonly}
				>
					<Select.Trigger class="w-full text-sm">
						{asString(effective(f)) || '— select —'}
					</Select.Trigger>
					<Select.Content>
						<Select.Item value="true" label="true" />
						<Select.Item value="false" label="false" />
					</Select.Content>
				</Select.Root>
			{:else if f.jsonType === 'integer' || f.jsonType === 'number'}
				<Input
					type="number"
					value={asString(effective(f))}
					placeholder={f.isSecret ? secretPlaceholder : undefined}
					disabled={readonly}
					oninput={(e) => {
						const raw = (e.currentTarget as HTMLInputElement).value;
						if (!coerceNumbers) {
							set(f.name, raw);
						} else if (raw === '') {
							set(f.name, undefined);
						} else {
							const n = f.jsonType === 'integer' ? parseInt(raw, 10) : parseFloat(raw);
							set(f.name, Number.isFinite(n) ? n : raw);
						}
					}}
					class="text-sm"
				/>
			{:else if f.isSecret}
				<Input
					type="password"
					value={asString(value[f.name])}
					placeholder={secretPlaceholder}
					disabled={readonly}
					oninput={(e) => set(f.name, (e.currentTarget as HTMLInputElement).value)}
					class="font-mono text-sm"
					data-testid="schema-form-secret-{f.name}"
				/>
			{:else}
				<Input
					type="text"
					value={asString(effective(f))}
					disabled={readonly}
					oninput={(e) => {
						const raw = (e.currentTarget as HTMLInputElement).value;
						// Editor mode: empty optional string → omit (matches the
						// hand-written panels' `value || undefined`). Resource modal
						// keeps the empty string (its submit path filters blanks).
						set(f.name, coerceNumbers && raw === '' && !f.isRequired ? undefined : raw);
					}}
					class="text-sm"
				/>
			{/if}
		</FormField>
	{/if}
{/each}
