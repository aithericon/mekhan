/**
 * config-spec/types.ts
 *
 * Unified vocabulary for the data-driven config-form layer.
 *
 * ConfigFieldKind is now defined as the UNION of:
 *   - FieldKind (the full 12-value canonical frontend vocabulary from
 *     $lib/fields/kind) — covers all value-input widget kinds.
 *   - AuthoringSlotKind ('ref' | 'resource' | 'code') — authoring-only
 *     slots that pick WHERE a value comes from (RefPicker, ResourcePicker,
 *     CodeEditor), not what kind of value it is. These are NOT canonical
 *     FieldKind values; FieldRenderer routes them to their own branches.
 *
 * The value-input subset now covers all 12 canonical kinds (previously 5).
 * The spike's per-kind field shapes (TextField / NumberField / …) are
 * preserved with field names aligned to FieldSpec where they overlap.
 */

import type { FieldKind } from '$lib/fields/kind';

// ---------------------------------------------------------------------------
// AuthoringSlotKind — three authoring-only slots (NOT value-input kinds)
// ---------------------------------------------------------------------------

/**
 * Authoring-only slot kinds that pick WHERE a value comes from, not what
 * widget to render for a data type. FieldRenderer handles these itself
 * (RefPicker / ResourcePicker / CodeEditor); they are never passed to FieldWidget.
 */
export type AuthoringSlotKind = 'ref' | 'resource' | 'code';

// ---------------------------------------------------------------------------
// ConfigFieldKind — the full config-spec vocabulary
// ---------------------------------------------------------------------------

/**
 * Union of canonical value-input kinds (FieldKind, 12 values) and the three
 * authoring-slot kinds. Adding a FieldKind automatically widens this union;
 * removing one causes exhaustive switches in FieldRenderer to fail the build.
 */
export type ConfigFieldKind = FieldKind | AuthoringSlotKind;

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

// ── Value-input kinds (delegate to FieldWidget) ──────────────────────────────

export type TextField = FieldBase & { kind: 'text'; placeholder?: string };
export type TextareaField = FieldBase & { kind: 'textarea'; rows?: number; placeholder?: string };
export type NumberField = FieldBase & {
	kind: 'number';
	min?: number;
	max?: number;
	step?: number;
	/**
	 * Transform applied before writing back to node-data.
	 * 'clamp01': clamp to [0,1] (ProgressUpdate fraction).
	 * 'optInt':  parse as int, emit undefined when blank.
	 * Config-spec-authoring concern — NOT carried by FieldWidget.
	 */
	transform?: 'clamp01' | 'optInt';
};
export type BoolField = FieldBase & { kind: 'bool' };
export type SelectField = FieldBase & { kind: 'select'; options: SelectOption[] };
// Canonical kinds added by widening from 5 to 12:
export type RadioField = FieldBase & { kind: 'radio'; options: SelectOption[] };
export type RangeField = FieldBase & { kind: 'range'; min?: number; max?: number; step?: number };
export type RatingField = FieldBase & { kind: 'rating'; maxRating?: number };
export type DateField = FieldBase & { kind: 'date'; includeTime?: boolean };
export type FileField = FieldBase & {
	kind: 'file';
	accept?: string;
	maxFiles?: number;
	maxFileSize?: number;
};
export type SignatureField = FieldBase & { kind: 'signature'; penColor?: string };
export type JsonField = FieldBase & { kind: 'json'; rows?: number; placeholder?: string };

// ── Authoring-slot kinds (handled by FieldRenderer directly, NOT FieldWidget) ─

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
	// value-input kinds (12)
	| TextField
	| TextareaField
	| NumberField
	| BoolField
	| SelectField
	| RadioField
	| RangeField
	| RatingField
	| DateField
	| FileField
	| SignatureField
	| JsonField
	// authoring-slot kinds (3)
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
