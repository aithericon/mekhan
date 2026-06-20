/**
 * Shared recursive schema model for the schema explorer/viewer.
 *
 * `SchemaNode` is a normalised, display-oriented view of a `TyDescriptor`,
 * a JSON Schema object, or a typed Port. It is flat enough to drive both the
 * read-only `SchemaView` (type tree, no values) and the value-annotated
 * `SchemaValueView` without either component knowing about the raw API shape.
 *
 * Keep this module pure: no imports from svelte, no side effects.
 */
import type { TyDescriptor } from '$lib/editor/guard-scope';
import { tyDescriptorLabel } from '$lib/editor/guard-scope';
import type { components } from '$lib/api/schema';

type PortField = components['schemas']['PortField'];
type Port = components['schemas']['Port'];

// ── SchemaNode ────────────────────────────────────────────────────────────────

export type SchemaNodeKind = 'object' | 'array' | 'scalar' | 'any' | 'opaque';

export type SchemaNode =
	| {
			kind: 'object';
			/** Ordered field name → child node entries. */
			fields: Map<string, SchemaNode>;
			/** True when the whole object is a pickable / selectable value. */
			selectable: boolean;
			label: string;
	  }
	| {
			kind: 'array';
			/** Schema of each array element. */
			element: SchemaNode;
			label: string;
	  }
	| {
			kind: 'scalar';
			/** e.g. "String", "Number", "Bool", "FileRef", "Timestamp" */
			name: string;
			label: string;
	  }
	| {
			kind: 'any';
			label: string;
	  }
	| {
			kind: 'opaque';
			name: string;
			label: string;
	  };

// ── Adapter ───────────────────────────────────────────────────────────────────

/**
 * Convert a `TyDescriptor` (from the backend analyzer / OpenAPI schema) into
 * the normalised `SchemaNode` model. Pure and recursive — safe to call at any
 * depth. Returns an `any`-typed node for `undefined` input so callers that
 * have an optional `ty` still get a valid node.
 */
export function tyDescriptorToSchemaNode(ty: TyDescriptor | undefined): SchemaNode {
	if (!ty) {
		return { kind: 'any', label: 'any' };
	}
	const label = tyDescriptorLabel(ty);
	switch (ty.kind) {
		case 'scalar':
			return { kind: 'scalar', name: ty.name, label };
		case 'object': {
			const fields = new Map<string, SchemaNode>();
			for (const [k, v] of Object.entries(ty.fields)) {
				fields.set(k, tyDescriptorToSchemaNode(v));
			}
			return { kind: 'object', fields, selectable: ty.selectable, label };
		}
		case 'array':
			return { kind: 'array', element: tyDescriptorToSchemaNode(ty.element), label };
		case 'any':
			return { kind: 'any', label };
		case 'opaque':
			return { kind: 'opaque', name: ty.name, label };
	}
}

// ── JSON Schema adapter ───────────────────────────────────────────────────────

const MAX_REF_DEPTH = 64;

/**
 * Convert a raw JSON Schema object into a `SchemaNode`.
 *
 * Mirrors the backend's `json_schema_to_token_shape` in
 * `service/src/compiler/token_shape/schema_parse.rs`:
 * - object/properties → object
 * - array/items → array
 * - type: string|number|integer|boolean → scalar (integer → Number)
 * - enum → scalar inferred from first value type
 * - $ref '#/definitions/X' or '#/$defs/X' → resolve against definitions
 *   (depth-capped at 64 to handle cycles)
 * - oneOf/anyOf/allOf / missing type / unparseable → any
 */
export function jsonSchemaToSchemaNode(
	schema: unknown,
	definitions?: Record<string, unknown>,
	_depth = 0
): SchemaNode {
	if (!schema || typeof schema !== 'object' || Array.isArray(schema)) {
		return { kind: 'any', label: 'any' };
	}
	const s = schema as Record<string, unknown>;

	// $ref resolution — '#/definitions/X' or '#/$defs/X'
	if (typeof s['$ref'] === 'string') {
		if (_depth >= MAX_REF_DEPTH) return { kind: 'any', label: 'any' };
		const ref = s['$ref'];
		const defsKey = ref.startsWith('#/definitions/')
			? ref.slice('#/definitions/'.length)
			: ref.startsWith('#/$defs/')
				? ref.slice('#/$defs/'.length)
				: null;
		if (defsKey && definitions && Object.prototype.hasOwnProperty.call(definitions, defsKey)) {
			return jsonSchemaToSchemaNode(definitions[defsKey], definitions, _depth + 1);
		}
		return { kind: 'any', label: 'any' };
	}

	// oneOf / anyOf / allOf — too complex for a flat tree, collapse to any
	if (s['oneOf'] || s['anyOf'] || s['allOf']) {
		return { kind: 'any', label: 'any' };
	}

	const type = s['type'];

	// object
	if (type === 'object' || (!type && typeof s['properties'] === 'object' && s['properties'] !== null)) {
		const props = s['properties'] as Record<string, unknown> | undefined;
		const fields = new Map<string, SchemaNode>();
		if (props) {
			for (const [k, v] of Object.entries(props)) {
				fields.set(k, jsonSchemaToSchemaNode(v, definitions, _depth + 1));
			}
		}
		const label = fields.size > 0 ? `{${[...fields.keys()].slice(0, 3).join(', ')}${fields.size > 3 ? ', …' : ''}}` : 'object';
		return { kind: 'object', fields, selectable: true, label };
	}

	// array
	if (type === 'array') {
		const items = s['items'];
		const element = items !== undefined
			? jsonSchemaToSchemaNode(items, definitions, _depth + 1)
			: { kind: 'any' as const, label: 'any' };
		return { kind: 'array', element, label: `array<${element.label}>` };
	}

	// enum — infer scalar from first value
	const enumVals = s['enum'];
	if (Array.isArray(enumVals) && enumVals.length > 0) {
		const first = enumVals[0];
		const name =
			typeof first === 'string' ? 'String'
			: typeof first === 'number' ? 'Number'
			: typeof first === 'boolean' ? 'Bool'
			: 'String';
		return { kind: 'scalar', name, label: name };
	}

	// scalars
	if (type === 'string') return { kind: 'scalar', name: 'String', label: 'String' };
	if (type === 'number') return { kind: 'scalar', name: 'Number', label: 'Number' };
	if (type === 'integer') return { kind: 'scalar', name: 'Number', label: 'Number' };
	if (type === 'boolean') return { kind: 'scalar', name: 'Bool', label: 'Bool' };

	return { kind: 'any', label: 'any' };
}

// ── File-metadata (catalogue) adapter ──────────────────────────────────────────

/** `{a, b, c, …}` style label for an object node. */
function objectNodeLabel(fields: Map<string, SchemaNode>): string {
	if (fields.size === 0) return 'object';
	const keys = [...fields.keys()];
	return `{${keys.slice(0, 3).join(', ')}${keys.length > 3 ? ', …' : ''}}`;
}

/**
 * Convert a raw `aithericon-file-metadata` `DataType` (as serialized into a
 * catalogue entry's `file_metadata.columns[].data_type`) into a `SchemaNode`.
 *
 * Mirrors the Rust enum's serde shape (`#[serde(rename_all = "snake_case")]`
 * in `shared/file-metadata/src/data_type.rs`):
 * - unit variants → bare strings: "string", "int64", "float64", "boolean", …
 * - `{ struct: [[name, dt], …] }`        → object (recursive)
 * - `{ list: dt }`                       → array
 * - `{ timestamp: { timezone } }`        → scalar (timezone folded into label)
 * - `{ dictionary: { index, value } }`   → the value's node
 * - `{ unknown: "x" }`                   → opaque
 */
export function fileMetadataDataTypeToSchemaNode(dt: unknown): SchemaNode {
	// Unit variants serialize as plain snake_case strings — keep them as the
	// label verbatim ("int64" vs "float64" is more informative than "Number").
	if (typeof dt === 'string') {
		return { kind: 'scalar', name: dt, label: dt };
	}
	if (!dt || typeof dt !== 'object' || Array.isArray(dt)) {
		return { kind: 'any', label: 'any' };
	}
	const obj = dt as Record<string, unknown>;
	if (Array.isArray(obj.struct)) {
		const fields = new Map<string, SchemaNode>();
		for (const pair of obj.struct as unknown[]) {
			if (Array.isArray(pair) && pair.length === 2 && typeof pair[0] === 'string') {
				fields.set(pair[0], fileMetadataDataTypeToSchemaNode(pair[1]));
			}
		}
		return { kind: 'object', fields, selectable: true, label: objectNodeLabel(fields) };
	}
	if ('list' in obj) {
		const element = fileMetadataDataTypeToSchemaNode(obj.list);
		return { kind: 'array', element, label: `list<${element.label}>` };
	}
	if ('timestamp' in obj) {
		const tz = (obj.timestamp as Record<string, unknown> | null)?.timezone;
		return { kind: 'scalar', name: 'timestamp', label: typeof tz === 'string' ? `timestamp<${tz}>` : 'timestamp' };
	}
	if ('dictionary' in obj) {
		return fileMetadataDataTypeToSchemaNode((obj.dictionary as Record<string, unknown> | null)?.value);
	}
	if ('unknown' in obj) {
		const name = typeof obj.unknown === 'string' ? obj.unknown : 'unknown';
		return { kind: 'opaque', name, label: name };
	}
	return { kind: 'any', label: 'any' };
}

/**
 * Build the per-record object `SchemaNode` from a catalogue entry's raw
 * `file_metadata.columns`. Returns null when no usable column schema exists
 * (legacy / pre-probe rows, or a non-record file).
 *
 * The probe stores one column per record field — for a single-object JSON those
 * are the object's keys, for an array-of-objects they are the element keys. Both
 * share this record shape, so the caller (which has the parsed value) decides
 * whether the file's top-level value IS this record or an array OF it.
 */
export function catalogueColumnsToSchemaNode(columns: unknown): SchemaNode | null {
	if (!Array.isArray(columns) || columns.length === 0) return null;
	const fields = new Map<string, SchemaNode>();
	for (const col of columns) {
		if (!col || typeof col !== 'object') continue;
		const c = col as Record<string, unknown>;
		if (typeof c.name !== 'string') continue;
		fields.set(c.name, fileMetadataDataTypeToSchemaNode(c.data_type));
	}
	if (fields.size === 0) return null;
	return { kind: 'object', fields, selectable: true, label: objectNodeLabel(fields) };
}

// ── Port adapter ──────────────────────────────────────────────────────────────

/**
 * Build an object `SchemaNode` from a typed `Port`.
 *
 * For each `PortField`:
 * - If it carries a `.schema`, that JSON Schema is the authoritative shape
 *   (used for `json`-kind fields with a rich override declared by the backend).
 * - Otherwise the flat `FieldKind` is mapped to a scalar SchemaNode, mirroring
 *   the backend's `ScalarTy::from_kind`:
 *     text / textarea / select / signature → String
 *     number → Number
 *     bool → Bool
 *     file → FileRef
 *     timestamp → Timestamp
 *     json → any
 */
export function portToSchemaNode(port: Port): SchemaNode {
	const fields = new Map<string, SchemaNode>();
	for (const f of port.fields ?? []) {
		fields.set(f.name, portFieldToSchemaNode(f));
	}
	const label =
		fields.size > 0
			? `{${[...fields.keys()].slice(0, 3).join(', ')}${fields.size > 3 ? ', …' : ''}}`
			: 'object';
	return { kind: 'object', fields, selectable: true, label };
}

function portFieldToSchemaNode(f: PortField): SchemaNode {
	if (f.schema !== undefined && f.schema !== null) {
		return jsonSchemaToSchemaNode(f.schema);
	}
	switch (f.kind) {
		case 'text':
		case 'textarea':
		case 'select':
		case 'signature':
			return { kind: 'scalar', name: 'String', label: 'String' };
		case 'number':
			return { kind: 'scalar', name: 'Number', label: 'Number' };
		case 'bool':
			return { kind: 'scalar', name: 'Bool', label: 'Bool' };
		case 'file':
			return { kind: 'scalar', name: 'FileRef', label: 'FileRef' };
		case 'timestamp':
			return { kind: 'scalar', name: 'Timestamp', label: 'Timestamp' };
		case 'json':
		default:
			return { kind: 'any', label: 'any' };
	}
}

// ── Value-heuristics (shared between KeyValueList and SchemaValueView) ────────

/**
 * True when the value is a primitive (string / number / boolean / null /
 * undefined) — safe to render directly without recursion.
 */
export function isPrimitive(v: unknown): boolean {
	return v === null || v === undefined || typeof v !== 'object';
}

/**
 * True when the value is a catalogue file reference `{url, filename?, ...}`.
 * Matches the heuristic used throughout the existing renderers.
 */
export function isFileRef(v: unknown): boolean {
	return (
		!!v &&
		typeof v === 'object' &&
		!Array.isArray(v) &&
		typeof (v as Record<string, unknown>).url === 'string'
	);
}

/**
 * True when the value is an S3 storage key the backend can serve at
 * `/api/v1/files/{key}`. Matches the heuristic in `KeyValueList` and
 * `index.ts`.
 */
export function isStorageKey(v: unknown): boolean {
	return typeof v === 'string' && /^(instances|templates|artifacts)\/\S+\.\w+$/.test(v);
}
