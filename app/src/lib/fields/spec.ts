/**
 * Canonical FieldSpec — the unified per-field descriptor used by FieldWidget
 * and defaultValueForKind.
 *
 * This is the single source of truth for per-kind configuration across the
 * three wire vocabularies (port FieldKind, TaskFieldKind, JsonType). Consumers
 * build a FieldSpec from whatever wire shape they hold and hand it to
 * FieldWidget for rendering.
 *
 * NOTE: not every field is relevant to every kind; irrelevant fields are
 * simply ignored by FieldWidget. This is intentional — a union of all
 * per-kind options is far less fragile than a discriminated union.
 */

import type { FieldKind } from './kind';

export type SelectOption = { value: string; label: string };

export type FieldSpec = {
	// ── identity ──────────────────────────────────────────────────
	/** API name / form key — used as the HTML `id` / `data-testid` base. */
	name: string;
	/** Canonical kind — drives widget selection. */
	kind: FieldKind;

	// ── common ────────────────────────────────────────────────────
	label?: string;
	description?: string;
	required?: boolean;
	readonly?: boolean;
	placeholder?: string;

	// ── select / radio ────────────────────────────────────────────
	options?: SelectOption[];

	// ── textarea ──────────────────────────────────────────────────
	rows?: number;

	// ── file ──────────────────────────────────────────────────────
	/** HTML accept string (e.g. "image/*,.pdf"). */
	accept?: string;
	maxFiles?: number;
	maxFileSize?: number;

	// ── number / range / rating ───────────────────────────────────
	min?: number;
	max?: number;
	step?: number;
	/** Rating-specific maximum (defaults to 5). */
	maxRating?: number;
	/**
	 * When true the field was derived from a JSON Schema `integer` type;
	 * callers use parseInt instead of parseFloat. The canonical kind stays
	 * 'number' in either case.
	 */
	integer?: boolean;

	// ── date ──────────────────────────────────────────────────────
	/** When true, include a time input beside the calendar. */
	includeTime?: boolean;

	// ── signature ─────────────────────────────────────────────────
	penColor?: string;

	// ── json / schema ─────────────────────────────────────────────
	/** Richer JSON Schema override (editor-internal; display-only). */
	schema?: unknown;

	// ── secret / password ─────────────────────────────────────────
	secret?: boolean;
	secretPlaceholder?: string;
};

// ---------------------------------------------------------------------------
// defaultValueForKind — single-sourced seed value for a new field.
// Exhaustive over all 12 canonical kinds.
// ---------------------------------------------------------------------------

/**
 * Return the appropriate zero-value for a given FieldKind so callers
 * (CreateInstanceDialog, TaskForm, FieldWidget hosts) have one canonical
 * seed rather than per-host `defaultForKind` copies.
 *
 * range / rating use '' — they stay string-stored until submit-time coercion
 * (coerceFormData in task-form-values) converts them to numbers.
 */
export function defaultValueForKind(kind: FieldKind): unknown {
	switch (kind) {
		case 'text':
			return '';
		case 'textarea':
			return '';
		case 'number':
			return 0;
		case 'bool':
			return false;
		case 'select':
			return '';
		case 'radio':
			return '';
		case 'range':
			// String-stored until submit-time coercion.
			return '';
		case 'rating':
			// String-stored until submit-time coercion.
			return '';
		case 'date':
			return '';
		case 'file':
			return null;
		case 'signature':
			return '';
		case 'json':
			return '';
		default: {
			// Exhaustiveness guard — fails the build if a new FieldKind is
			// added without updating this switch.
			const _exhaustive: never = kind;
			throw new Error(`defaultValueForKind: unmapped FieldKind "${_exhaustive}"`);
		}
	}
}
