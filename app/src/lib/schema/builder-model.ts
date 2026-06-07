/**
 * Faithful editable tree model for JSON Schema authoring.
 *
 * SchemaNode (in model.ts) is display-only and lossy — no required/description/
 * default/constraints. The builder works with BuilderNode instead, which
 * preserves full JSON-Schema fidelity and round-trips losslessly through the
 * schema<->tree mapping.
 *
 * "Lossless" scope: everything the builder supports (object/array/scalars,
 * enum, required, description, default, nullable, numeric/string constraints,
 * x-field-kind, oneOf/anyOf unions, $ref to named definitions).
 * Constructs the builder does NOT support (allOf, not, patternProperties) →
 * detected and left as RAW-ONLY nodes.
 *
 * Keep this module pure: no imports from svelte, no side effects.
 */

// ── canonical scalar type vocabulary ──────────────────────────────────────────

/** Wire FieldKind values the builder can store as a hint on a scalar node. */
export type FieldKindHint =
	| 'text'
	| 'textarea'
	| 'number'
	| 'bool'
	| 'select'
	| 'file'
	| 'signature'
	| 'timestamp'
	| 'json';

/** JSON Schema scalar primitives representable in the builder. */
export type ScalarJsonType = 'string' | 'number' | 'integer' | 'boolean';

// ── BuilderNode union ─────────────────────────────────────────────────────────

/**
 * Editable tree node. Each variant maps 1-to-1 with a JSON Schema pattern.
 *
 * `nullable` is represented as a `type: [T, "null"]` array in JSON Schema and
 * stored as a boolean flag here for editing convenience.
 *
 * All meta fields (description, default) are shared across variants via the
 * `NodeMeta` interface inlined into each member.
 */
export type BuilderNode =
	| BuilderObjectNode
	| BuilderArrayNode
	| BuilderScalarNode
	| BuilderUnionNode
	| BuilderRefNode
	| BuilderRawNode;

export type BuilderObjectNode = {
	kind: 'object';
	/** Ordered list of property entries (preserves field order). */
	fields: BuilderField[];
	/** Properties in the JSON Schema `required` array. */
	required: Set<string>;
	title?: string;
	description?: string;
	/** When true: emit `additionalProperties: false`. */
	sealed: boolean;
	nullable: boolean;
};

export type BuilderArrayNode = {
	kind: 'array';
	/** Schema for array items. */
	items: BuilderNode;
	description?: string;
	nullable: boolean;
	minItems?: number;
	maxItems?: number;
};

export type BuilderScalarNode = {
	kind: 'scalar';
	type: ScalarJsonType;
	description?: string;
	default?: unknown;
	nullable: boolean;
	/** Enum values (JSON Schema `enum` array). */
	enumValues: string[];
	/** format hint (e.g. "textarea", "date-time"). */
	format?: string;
	/** `x-field-kind` extension stored on the schema node. */
	fieldKindHint?: FieldKindHint;
	/** Numeric constraints (for type: number / integer). */
	minimum?: number;
	maximum?: number;
	/** String constraints. */
	minLength?: number;
	maxLength?: number;
	pattern?: string;
};

/**
 * A union node (oneOf or anyOf). Each variant is a full sub-schema
 * editable via a nested SchemaBuilder.
 *
 * `discriminator` is an optional property name whose value is a single-value
 * enum (`{enum: ["tag_value"]}`) in each variant — the pattern used by the
 * SchemaForm renderer (serde internally-tagged-enum shape). When present the
 * UI surfaces the tag-value per variant rather than a generic index label.
 *
 * allOf is NOT supported here and stays raw-only.
 */
export type BuilderUnionNode = {
	kind: 'union';
	/** 'oneOf' or 'anyOf'. Defaults to 'oneOf' for new nodes. */
	combinator: 'oneOf' | 'anyOf';
	/** Ordered list of variant sub-schemas. */
	variants: BuilderNode[];
	description?: string;
	/**
	 * Optional discriminator property name. When set, the builder adds a
	 * const-tagged property (`{enum: ["<tag>"]}`) as the first property of
	 * each variant's object schema and uses the tag as the variant label.
	 * The discriminator name must be identical across all variants for the
	 * SchemaForm to detect it.
	 */
	discriminator?: string;
	/**
	 * Sibling keys present on the original union schema that the builder does
	 * not model explicitly (e.g. `title`, `default`, vendor extensions, or a
	 * second combinator). Preserved verbatim and re-emitted so the
	 * schema↔builder round-trip stays lossless. Omitted when there are none.
	 */
	extra?: Record<string, unknown>;
};

/**
 * A $ref node pointing at a named definition from the workflow's
 * `definitions` map. Round-trips losslessly as `{"$ref":"#/definitions/<name>"}`.
 * Also accepts `#/$defs/<name>` on the read path but always emits the
 * `#/definitions/` form (which is what the backend's `inline_refs` pass
 * recognises).
 */
export type BuilderRefNode = {
	kind: 'ref';
	/** The definition name (the part after `#/definitions/`). */
	name: string;
};

/** A node the builder cannot edit (contains allOf / not / patternProperties). */
export type BuilderRawNode = {
	kind: 'raw';
	/** The reason this node is raw-only. */
	reason: string;
	/** The raw JSON Schema value, preserved unchanged. */
	raw: unknown;
};

/** A named property in an object node. */
export type BuilderField = {
	/**
	 * Stable, UI-only identity for this field. Used as a keyed-list identity in
	 * the builder UI so editing a field's name doesn't remount its subtree.
	 * Never serialized to JSON Schema (see `builderNodeToSchema`, which reads
	 * only `name`/`node`).
	 */
	id: string;
	name: string;
	node: BuilderNode;
};

// ── UI-only field id generator ────────────────────────────────────────────────

let __fieldIdSeq = 0;

/**
 * Allocate a fresh UI-only field id. Deterministic, monotonic counter — no
 * clock reads, no randomness. Never serialized to JSON Schema.
 */
export function nextFieldId(): string {
	__fieldIdSeq += 1;
	return `bf${__fieldIdSeq}`;
}

// ── Detection helpers ─────────────────────────────────────────────────────────

/** Keys the builder cannot model — still force raw-only. */
const UNSUPPORTED_KEYS = ['allOf', 'not', 'patternProperties'] as const;

function detectUnsupported(s: Record<string, unknown>): string | null {
	for (const k of UNSUPPORTED_KEYS) {
		if (k in s) return `Uses \`${k}\` — edit as raw JSON.`;
	}
	return null;
}

// ── Union helper: detect discriminator ───────────────────────────────────────

/**
 * Given a list of variant sub-schemas (already parsed as objects), find the
 * discriminator property name. The discriminator is a property present in
 * every variant whose schema is `{enum: ["<single_value>"]}` (the serde
 * internally-tagged-enum pattern used by SchemaForm).
 *
 * Also accepts `{const: "tag"}` variants (less common in schemars output but
 * valid JSON Schema 2019-09+ equivalent). This is only used internally by
 * the builder's `schemaToBuilderNode` — the public `discriminatorOf` below
 * uses the enum-only check to match SchemaForm's exact detection logic.
 */
function detectDiscriminator(
	variants: Record<string, unknown>[]
): string | undefined {
	if (variants.length === 0) return undefined;
	const firstProps = variants[0]['properties'];
	if (!firstProps || typeof firstProps !== 'object' || Array.isArray(firstProps)) return undefined;
	const candidates = Object.keys(firstProps as Record<string, unknown>);
	for (const name of candidates) {
		const isDiscriminatorInAll = variants.every((v) => {
			const props = v['properties'];
			if (!props || typeof props !== 'object' || Array.isArray(props)) return false;
			const prop = (props as Record<string, unknown>)[name];
			if (!prop || typeof prop !== 'object' || Array.isArray(prop)) return false;
			const enumArr = (prop as Record<string, unknown>)['enum'];
			// Support both `{enum: ["tag"]}` and `{const: "tag"}` patterns.
			const isConstEnum = Array.isArray(enumArr) && enumArr.length === 1;
			const isConst = 'const' in (prop as Record<string, unknown>);
			return isConstEnum || isConst;
		});
		if (isDiscriminatorInAll) return name;
	}
	return undefined;
}

// ── Shared public helper ──────────────────────────────────────────────────────

/**
 * The discriminator field of a `oneOf`/`anyOf` schema, or `null` for a plain
 * object schema.
 *
 * A discriminator is a property present in EVERY variant with a single-value
 * `enum` (the serde internally-tagged-enum shape — e.g. a datacenter's
 * `scheduler_flavor`). This is the canonical check used by SchemaForm for
 * value-entry widget selection: it looks only at the `enum` pattern (not the
 * `const` shorthand) so its output is byte-for-byte identical to the
 * discriminator check SchemaForm previously performed internally.
 *
 * Exported here so SchemaForm can import it instead of duplicating the logic.
 */
export function discriminatorOf(
	schema: Record<string, unknown> | null | undefined
): string | null {
	if (!schema) return null;
	const oneOf = schema['oneOf'] ?? schema['anyOf'];
	if (!Array.isArray(oneOf) || oneOf.length === 0) return null;
	// Extract variant objects (skip non-object entries gracefully).
	const variants = oneOf.map((v) => ({
		props: ((v as Record<string, unknown>)['properties'] ?? {}) as Record<
			string,
			Record<string, unknown>
		>,
		required: (((v as Record<string, unknown>)['required'] ?? []) as string[]) ?? []
	}));
	for (const name of Object.keys(variants[0].props)) {
		const constInAll = variants.every((v) => {
			const q = v.props[name];
			return Array.isArray(q?.['enum']) && (q['enum'] as unknown[]).length === 1;
		});
		if (constInAll) return name;
	}
	return null;
}

// ── schemaToBuilderNode ───────────────────────────────────────────────────────

/**
 * Parse a raw JSON Schema object into a `BuilderNode`.
 *
 * Returns a `BuilderRawNode` for schemas that contain constructs the builder
 * cannot round-trip ($ref, oneOf, anyOf, allOf, not, patternProperties, or
 * array of `type` values other than `[T, "null"]`).
 *
 * Accepts `null` / `undefined` / non-object → empty object node (useful for
 * blank port-field sub-schemas).
 */
export function schemaToBuilderNode(schema: unknown): BuilderNode {
	if (schema == null || typeof schema !== 'object' || Array.isArray(schema)) {
		// Empty / absent schema → treat as empty object (editable blank slate).
		return { kind: 'object', fields: [], required: new Set(), sealed: false, nullable: false };
	}

	const s = schema as Record<string, unknown>;

	// Empty object → editable blank slate.
	if (Object.keys(s).length === 0) {
		return { kind: 'object', fields: [], required: new Set(), sealed: false, nullable: false };
	}

	// ── $ref — handled directly ───────────────────────────────────────────────
	if (typeof s['$ref'] === 'string') {
		const ref = s['$ref'];
		// Accept both #/definitions/X and #/$defs/X on the read path.
		const defsKey = ref.startsWith('#/definitions/')
			? ref.slice('#/definitions/'.length)
			: ref.startsWith('#/$defs/')
				? ref.slice('#/$defs/'.length)
				: null;
		if (defsKey !== null) {
			return { kind: 'ref', name: defsKey };
		}
		// External ref — leave as raw.
		return { kind: 'raw', reason: `External \`$ref\` (${ref}) — edit as raw JSON.`, raw: schema };
	}

	// ── oneOf / anyOf — handled directly ─────────────────────────────────────
	if ('oneOf' in s || 'anyOf' in s) {
		const combinator: 'oneOf' | 'anyOf' = 'oneOf' in s ? 'oneOf' : 'anyOf';
		const rawVariants = s[combinator];
		if (!Array.isArray(rawVariants)) {
			return { kind: 'raw', reason: `\`${combinator}\` is not an array — edit as raw JSON.`, raw: schema };
		}
		const variants = rawVariants.map((v) => schemaToBuilderNode(v));
		// Detect discriminator from the raw variant objects (not the parsed nodes).
		const rawObjs = rawVariants.filter(
			(v): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v)
		);
		const discriminator = rawObjs.length === rawVariants.length
			? detectDiscriminator(rawObjs)
			: undefined;
		const description = typeof s['description'] === 'string' ? s['description'] : undefined;
		// Preserve any sibling keys we don't model (title, default, a second
		// combinator, vendor extensions) so the round-trip stays lossless.
		const extraEntries = Object.entries(s).filter(
			([k]) => k !== combinator && k !== 'description'
		);
		const extra = extraEntries.length > 0 ? Object.fromEntries(extraEntries) : undefined;
		return { kind: 'union', combinator, variants, description, discriminator, ...(extra ? { extra } : {}) };
	}

	const unsup = detectUnsupported(s);
	if (unsup) {
		return { kind: 'raw', reason: unsup, raw: schema };
	}

	// Resolve nullable: type may be a two-element array like ["string", "null"].
	let nullable = false;
	let effectiveType = s['type'];
	if (Array.isArray(effectiveType)) {
		const withoutNull = effectiveType.filter((t) => t !== 'null');
		if (withoutNull.length === 1 && effectiveType.includes('null')) {
			nullable = true;
			effectiveType = withoutNull[0];
		} else {
			// Multi-type array that isn't [T, "null"] → raw only.
			return {
				kind: 'raw',
				reason: 'Uses a `type` array with multiple non-null types — edit as raw JSON.',
				raw: schema
			};
		}
	}

	const description =
		typeof s['description'] === 'string' ? (s['description'] as string) : undefined;

	// ── object ────────────────────────────────────────────────────────────────
	if (
		effectiveType === 'object' ||
		(!effectiveType && typeof s['properties'] === 'object' && s['properties'] !== null)
	) {
		const rawProps = s['properties'] as Record<string, unknown> | undefined;
		const requiredArr = Array.isArray(s['required']) ? (s['required'] as string[]) : [];
		const fields: BuilderField[] = [];

		if (rawProps) {
			for (const [name, propSchema] of Object.entries(rawProps)) {
				const childNode = schemaToBuilderNode(propSchema);
				fields.push({ id: nextFieldId(), name, node: childNode });
			}
		}

		return {
			kind: 'object',
			fields,
			required: new Set(requiredArr),
			title: typeof s['title'] === 'string' ? (s['title'] as string) : undefined,
			description,
			sealed: s['additionalProperties'] === false,
			nullable
		};
	}

	// ── array ────────────────────────────────────────────────────────────────
	if (effectiveType === 'array') {
		const itemsSchema = s['items'];
		const items =
			itemsSchema !== undefined
				? schemaToBuilderNode(itemsSchema)
				: ({ kind: 'object', fields: [], required: new Set(), sealed: false, nullable: false } as BuilderObjectNode);

		return {
			kind: 'array',
			items,
			description,
			nullable,
			minItems: typeof s['minItems'] === 'number' ? (s['minItems'] as number) : undefined,
			maxItems: typeof s['maxItems'] === 'number' ? (s['maxItems'] as number) : undefined
		};
	}

	// ── scalar ───────────────────────────────────────────────────────────────
	if (
		effectiveType === 'string' ||
		effectiveType === 'number' ||
		effectiveType === 'integer' ||
		effectiveType === 'boolean'
	) {
		const enumRaw = s['enum'];
		const enumValues: string[] = Array.isArray(enumRaw)
			? enumRaw.map((v) => String(v))
			: [];

		// Field-kind hint stored as x-field-kind extension.
		const fieldKindHint =
			typeof s['x-field-kind'] === 'string'
				? (s['x-field-kind'] as FieldKindHint)
				: undefined;

		return {
			kind: 'scalar',
			type: effectiveType as ScalarJsonType,
			description,
			default: s['default'],
			nullable,
			enumValues,
			format: typeof s['format'] === 'string' ? (s['format'] as string) : undefined,
			fieldKindHint,
			minimum: typeof s['minimum'] === 'number' ? (s['minimum'] as number) : undefined,
			maximum: typeof s['maximum'] === 'number' ? (s['maximum'] as number) : undefined,
			minLength: typeof s['minLength'] === 'number' ? (s['minLength'] as number) : undefined,
			maxLength: typeof s['maxLength'] === 'number' ? (s['maxLength'] as number) : undefined,
			pattern: typeof s['pattern'] === 'string' ? (s['pattern'] as string) : undefined
		};
	}

	// Unrecognised shape → raw.
	return {
		kind: 'raw',
		reason: `Unrecognised schema shape (type=\`${String(effectiveType ?? 'none')}\`) — edit as raw JSON.`,
		raw: schema
	};
}

// ── builderNodeToSchema ───────────────────────────────────────────────────────

/**
 * Serialise a `BuilderNode` back to a plain JSON Schema object.
 *
 * Contract:
 *   - All fields the builder understands are emitted faithfully.
 *   - `BuilderRawNode.raw` is returned as-is (pass-through).
 *   - Empty strings for optional string fields are omitted.
 *   - Numeric constraint `undefined` values are omitted.
 *   - `nullable: true` emits `type: [base, "null"]`.
 *   - `sealed: true` emits `additionalProperties: false`.
 *   - `required` array is emitted only when non-empty and sorted for
 *     deterministic output.
 */
export function builderNodeToSchema(node: BuilderNode): unknown {
	switch (node.kind) {
		case 'raw':
			return node.raw;

		case 'object': {
			const properties: Record<string, unknown> = {};
			for (const field of node.fields) {
				if (field.name) {
					properties[field.name] = builderNodeToSchema(field.node);
				}
			}
			const out: Record<string, unknown> = {
				type: node.nullable ? ['object', 'null'] : 'object'
			};
			if (node.title) out['title'] = node.title;
			if (node.description) out['description'] = node.description;
			if (Object.keys(properties).length > 0) out['properties'] = properties;
			const req = [...node.required].filter((r) =>
				node.fields.some((f) => f.name === r)
			).sort();
			if (req.length > 0) out['required'] = req;
			if (node.sealed) out['additionalProperties'] = false;
			return out;
		}

		case 'array': {
			const out: Record<string, unknown> = {
				type: node.nullable ? ['array', 'null'] : 'array',
				items: builderNodeToSchema(node.items)
			};
			if (node.description) out['description'] = node.description;
			if (node.minItems !== undefined) out['minItems'] = node.minItems;
			if (node.maxItems !== undefined) out['maxItems'] = node.maxItems;
			return out;
		}

		case 'ref':
			return { $ref: `#/definitions/${node.name}` };

		case 'union': {
			const variantSchemas = node.variants.map(builderNodeToSchema);
			// Re-emit preserved sibling keys first so the modelled keys
			// (combinator, description) take precedence over any stale copies.
			const out: Record<string, unknown> = { ...(node.extra ?? {}) };
			out[node.combinator] = variantSchemas;
			if (node.description) out['description'] = node.description;
			return out;
		}

		case 'scalar': {
			const baseType = node.type;
			const out: Record<string, unknown> = {
				type: node.nullable ? [baseType, 'null'] : baseType
			};
			if (node.description) out['description'] = node.description;
			if (node.default !== undefined) out['default'] = node.default;
			if (node.format) out['format'] = node.format;
			if (node.fieldKindHint) out['x-field-kind'] = node.fieldKindHint;
			if (node.enumValues.length > 0) {
				// Coerce enum values to the correct JS type for number/integer/boolean.
				out['enum'] = node.enumValues.map((v) => coerceEnumValue(v, node.type));
			}
			// Numeric constraints.
			if (node.minimum !== undefined) out['minimum'] = node.minimum;
			if (node.maximum !== undefined) out['maximum'] = node.maximum;
			// String constraints.
			if (node.minLength !== undefined) out['minLength'] = node.minLength;
			if (node.maxLength !== undefined) out['maxLength'] = node.maxLength;
			if (node.pattern) out['pattern'] = node.pattern;
			return out;
		}
	}
}

function coerceEnumValue(v: string, type: ScalarJsonType): unknown {
	if (type === 'number') return Number(v);
	if (type === 'integer') return parseInt(v, 10);
	if (type === 'boolean') return v === 'true';
	return v;
}

// ── Convenience: detect if a BuilderNode subtree has any raw nodes ────────────

export function hasRawDescendant(node: BuilderNode): boolean {
	switch (node.kind) {
		case 'raw':
			return true;
		case 'object':
			return node.fields.some((f) => hasRawDescendant(f.node));
		case 'array':
			return hasRawDescendant(node.items);
		case 'union':
			return node.variants.some(hasRawDescendant);
		case 'ref':
		case 'scalar':
			return false;
	}
}

// ── uniqueFieldName — generate a non-colliding name ──────────────────────────

export function uniqueFieldName(existing: string[]): string {
	const used = new Set(existing);
	let i = existing.length + 1;
	let name = `field_${i}`;
	while (used.has(name)) {
		i += 1;
		name = `field_${i}`;
	}
	return name;
}

// ── slugifyFieldName — sanitise a user-typed name ────────────────────────────

export function slugifyFieldName(label: string): string {
	return label
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '_')
		.replace(/^_|_$/g, '');
}
