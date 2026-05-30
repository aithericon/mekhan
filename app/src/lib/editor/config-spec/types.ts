/**
 * SPIKE — config-spec/types.ts
 *
 * Unified vocabulary for the data-driven config-form layer.  This is an
 * ADDITIVE prototype that lives alongside the three existing field-kind enums
 * (FieldKind from schema.d.ts, TaskFieldKind from hpi/types.ts, JsonType from
 * SchemaForm.svelte) without modifying any of them.
 *
 * Intended migration path (post-spike):
 *   1. Fold FieldKind / TaskFieldKind / JsonType into ConfigFieldKind where
 *      overlap exists; add missing variants (e.g. 'ref', 'resource', 'code').
 *   2. Replace bespoke section components one node-type at a time.
 *   3. Remove the three legacy enums once all consumers are migrated.
 */

import type { ScopeEntry } from '$lib/editor/guard-scope';

// ---------------------------------------------------------------------------
// ConfigFieldKind — the spike superset of all widget vocabulary
// ---------------------------------------------------------------------------

export type ConfigFieldKind =
	| 'text' // single-line text input
	| 'textarea' // multi-line text input; may carry `{{ ref }}` interpolation
	| 'number' // numeric input (integer or float)
	| 'bool' // checkbox or toggle
	| 'select' // dropdown; requires `options`
	| 'ref' // RefPicker — emits a qualified `<slug>.<field>` string
	| 'resource' // ResourcePicker — binds a workspace resource alias
	| 'code'; // CodeEditor — Rhai / Python / JSON source

// ---------------------------------------------------------------------------
// Per-kind extras (discriminated union carries only what each kind needs)
// ---------------------------------------------------------------------------

export type SelectOption = { value: string; label: string };

type FieldBase = {
	/** Key used to read/write the value inside the node-data object. */
	bind: string;
	/** Human-readable label shown above the field. */
	label: string;
	/** Optional hint shown below the label. */
	description?: string;
	/** When true the field is rendered but not editable. */
	readonly?: boolean;
};

export type TextField = FieldBase & { kind: 'text'; placeholder?: string };
export type TextareaField = FieldBase & { kind: 'textarea'; rows?: number; placeholder?: string };
export type NumberField = FieldBase & {
	kind: 'number';
	min?: number;
	max?: number;
	step?: number;
	/** Transform applied before writing back: 'clamp01' restricts 0..1. */
	transform?: 'clamp01' | 'optInt';
};
export type BoolField = FieldBase & { kind: 'bool' };
export type SelectField = FieldBase & { kind: 'select'; options: SelectOption[] };
export type RefField = FieldBase & {
	kind: 'ref';
	/**
	 * When true the picker surfaces the `[*]` array-boundary synthetic child
	 * (Feature B). Defaults to false — safe for guards/conditions.
	 */
	allowArrayBoundary?: boolean;
	placeholder?: string;
};
export type ResourceField = FieldBase & {
	kind: 'resource';
	/** Passed to ResourcePicker.resourceType — `null` renders nothing. */
	resourceType: string;
	label?: string;
	typeLabel?: string;
};
export type CodeField = FieldBase & {
	kind: 'code';
	lang: 'python' | 'rhai' | 'json';
	minHeight?: string;
	maxHeight?: string;
};

export type ConfigFieldSpec =
	| TextField
	| TextareaField
	| NumberField
	| BoolField
	| SelectField
	| RefField
	| ResourceField
	| CodeField;

// ---------------------------------------------------------------------------
// NodeConfigSpec — the full spec for one node type
// ---------------------------------------------------------------------------

export type NodeConfigSpec = {
	fields: ConfigFieldSpec[];
};

// ---------------------------------------------------------------------------
// get/set helpers — flat key access for the spike.
//
// TODO: nested dot-path support (`a.b.c`) is a natural extension; add a
// recursive descent here when sub-object fields need to be managed by the
// framework rather than by a nested SchemaDrivenSection.
// ---------------------------------------------------------------------------

/** Read a top-level key from the node-data object (typed loosely for the spike). */
export function getByBind(data: Record<string, unknown>, bind: string): unknown {
	return data[bind];
}

/** Return a new node-data object with the given top-level key updated. */
export function setByBind(
	data: Record<string, unknown>,
	bind: string,
	value: unknown
): Record<string, unknown> {
	return { ...data, [bind]: value };
}
