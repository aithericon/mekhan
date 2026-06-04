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
 * x-field-kind). Constructs the builder does NOT support ($ref, oneOf, anyOf,
 * allOf) → detected and left as RAW-ONLY nodes.
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

/** A node the builder cannot edit (contains $ref / oneOf / anyOf / allOf). */
export type BuilderRawNode = {
	kind: 'raw';
	/** The reason this node is raw-only. */
	reason: string;
	/** The raw JSON Schema value, preserved unchanged. */
	raw: unknown;
};

/** A named property in an object node. */
export type BuilderField = {
	name: string;
	node: BuilderNode;
};

// ── Detection helpers ─────────────────────────────────────────────────────────

const UNSUPPORTED_KEYS = ['$ref', 'anyOf', 'oneOf', 'allOf', 'not', 'patternProperties'] as const;

function detectUnsupported(s: Record<string, unknown>): string | null {
	for (const k of UNSUPPORTED_KEYS) {
		if (k in s) return `Uses \`${k}\` — edit as raw JSON.`;
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
				fields.push({ name, node: childNode });
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
