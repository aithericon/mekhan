/**
 * config-spec/types.ts
 *
 * Unified vocabulary for the data-driven config-form layer.
 *
 * ConfigFieldKind is now defined as the UNION of:
 *   - FieldKind (the full 12-value canonical frontend vocabulary from
 *     $lib/fields/kind) — covers all value-input widget kinds.
 *   - AuthoringSlotKind ('ref' | 'resource' | 'code' | 'port') — authoring-only
 *     slots that pick WHERE a value comes from (RefPicker, ResourcePicker,
 *     CodeEditor, PortsSection), not what kind of value it is. These are NOT
 *     canonical FieldKind values; FieldRenderer routes them to their own branches.
 *
 * The value-input subset now covers all 12 canonical kinds (previously 5).
 * The spike's per-kind field shapes (TextField / NumberField / …) are
 * preserved with field names aligned to FieldSpec where they overlap.
 */

import type { FieldKind } from '$lib/fields/kind';
import type { components } from '$lib/api/schema';

/** The Port schema type from the generated OpenAPI schema. */
export type Port = components['schemas']['Port'];

// ---------------------------------------------------------------------------
// AuthoringSlotKind — four authoring-only slots (NOT value-input kinds)
// ---------------------------------------------------------------------------

/**
 * Authoring-only slot kinds that pick WHERE a value comes from, not what
 * widget to render for a data type. FieldRenderer handles these itself
 * (RefPicker / ResourcePicker / CodeEditor / PortsSection); they are never
 * passed to FieldWidget.
 */
export type AuthoringSlotKind = 'ref' | 'resource' | 'code' | 'port';

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

export type TextField = FieldBase & {
	kind: 'text';
	placeholder?: string;
	/**
	 * When true the text input renders with font-mono.
	 * Used for itemVar / resultVar in MAP_SPEC (bespoke section preserved that class).
	 */
	mono?: boolean;
	/**
	 * Value shown in the input when data[bind] is undefined/absent.
	 * Used for itemVar which binds value={data.itemVar ?? 'item'} — the live
	 * fallback is 'item', not just a placeholder (which sits in grey text).
	 * This default is for DISPLAY only; it is NOT written back to data on mount.
	 * Writes from oninput still store whatever the user typed verbatim.
	 */
	valueDefault?: string;
};
export type TextareaField = FieldBase & {
	kind: 'textarea';
	rows?: number;
	placeholder?: string;
	/**
	 * When true an empty string is collapsed to `undefined` before writing back.
	 * Required for optional message/failureMessage fields that must not store ''.
	 * Matches the bespoke `v === '' ? undefined : v` guard.
	 */
	clearToUndefined?: boolean;
};
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
export type SelectField = FieldBase & {
	kind: 'select';
	options: SelectOption[];
	/**
	 * Value shown in the trigger when the data key is undefined/absent.
	 * Used for phase_update.status which defaults to 'running' for display
	 * WITHOUT writing that default into node data on mount.
	 * FieldRenderer uses this as a read-through fallback only (display, not write).
	 */
	displayDefault?: string;
};
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

/**
 * Authoring-slot: renders the existing PortsSection.svelte for editing a
 * single named Port (add/remove/rename typed fields). NOT a value-input kind.
 *
 * Used by MAP_SPEC for the `output` (Element shape) port editor. The Port
 * value is read from `data[bind]`; when absent, `default` (or the built-in
 * sentinel { id: 'out', label: 'Element', fields: [] }) is used so
 * PortsSection always receives a valid Port to edit.
 *
 * FieldRenderer writes the WHOLE edited Port verbatim back via onchange
 * without coercion; an empty-fields Port is NOT collapsed to undefined.
 */
export type PortField = FieldBase & {
	kind: 'port';
	/** Passed as the `title` prop to PortsSection. */
	title?: string;
	/** Passed as the `emptyHint` prop to PortsSection. */
	emptyHint?: string;
	/** Synthesized Port when `data[bind]` is unset. */
	default?: Port;
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
	// authoring-slot kinds (4)
	| RefField
	| ResourceField
	| CodeField
	| PortField;

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
