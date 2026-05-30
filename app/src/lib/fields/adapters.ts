/**
 * Total, exhaustive adapters from each wire vocabulary to canonical FieldKind.
 *
 * Each switch ends in a `never` default — an unmapped wire variant causes a
 * TypeScript compile error, enforcing that all three adapter tables stay in
 * sync with their respective wire enums.
 *
 * WIRE vs CANONICAL:
 *   Port FieldKind (9 values)  — components['schemas']['FieldKind'] in schema.d.ts
 *   Task TaskFieldKind (11 values) — TASK_FIELD_KINDS in hpi/types.ts
 *   SchemaForm JsonType (7 values) — JsonType in SchemaForm.svelte
 *
 * None of these wire enums are renamed; adapters convert wire→canonical for
 * widget selection only. Authoring setters continue to write wire values.
 */

import type { FieldKind } from './kind';
import type { components } from '$lib/api/schema';

/** Wire type alias for port fields (from schema.d.ts — never hand-edit). */
export type PortFieldKind = components['schemas']['FieldKind'];

/** Wire type alias for HPI human-task fields (from schema.d.ts — never hand-edit). */
export type TaskFieldKindWire = components['schemas']['TaskFieldKind'];

/** JSON Schema primitive types as understood by SchemaForm.svelte. */
export type JsonType =
	| 'string'
	| 'integer'
	| 'number'
	| 'boolean'
	| 'array'
	| 'object'
	| 'unknown';

// ---------------------------------------------------------------------------
// fromPortFieldKind — 9 wire values → canonical FieldKind
// ---------------------------------------------------------------------------

/**
 * Map a port-side FieldKind wire value (text/textarea/number/bool/select/
 * file/signature/timestamp/json) to the canonical frontend FieldKind.
 *
 * NOTE: 'timestamp' maps to 'date' canonically, but MUST NOT be written back
 * to a PortField — keep writing 'timestamp' when authoring port kinds.
 */
export function fromPortFieldKind(k: PortFieldKind): FieldKind {
	switch (k) {
		case 'text':
			return 'text';
		case 'textarea':
			return 'textarea';
		case 'number':
			return 'number';
		case 'bool':
			return 'bool';
		case 'select':
			return 'select';
		case 'file':
			return 'file';
		case 'signature':
			return 'signature';
		case 'timestamp':
			// Canonical 'date'; callers that write back to PortField must use
			// the original wire value 'timestamp', not the canonical 'date'.
			return 'date';
		case 'json':
			return 'json';
		default: {
			// Exhaustiveness guard — fails the build if a new PortFieldKind
			// variant is added to the wire schema without updating this adapter.
			const _exhaustive: never = k;
			throw new Error(`fromPortFieldKind: unmapped wire value "${_exhaustive}"`);
		}
	}
}

// ---------------------------------------------------------------------------
// fromTaskFieldKind — 11 wire values → canonical FieldKind
// ---------------------------------------------------------------------------

/**
 * Map a HPI TaskFieldKind wire value to the canonical frontend FieldKind.
 *
 * Notable mappings:
 *   checkbox → bool  (same checkbox widget, canonical name aligned with port side)
 *   radio, range, rating → distinct canonical kinds (kept separate, distinct widgets)
 *   date → date
 */
export function fromTaskFieldKind(k: TaskFieldKindWire): FieldKind {
	switch (k) {
		case 'text':
			return 'text';
		case 'textarea':
			return 'textarea';
		case 'number':
			return 'number';
		case 'select':
			return 'select';
		case 'checkbox':
			// HPI wire value 'checkbox' → canonical 'bool'
			return 'bool';
		case 'file':
			return 'file';
		case 'signature':
			return 'signature';
		case 'radio':
			return 'radio';
		case 'date':
			return 'date';
		case 'range':
			return 'range';
		case 'rating':
			return 'rating';
		default: {
			const _exhaustive: never = k;
			throw new Error(`fromTaskFieldKind: unmapped wire value "${_exhaustive}"`);
		}
	}
}

// ---------------------------------------------------------------------------
// fromJsonType — 7 JsonType values → canonical FieldKind
// ---------------------------------------------------------------------------

/**
 * Map a JSON Schema primitive type to canonical FieldKind.
 *
 * `opts.hasEnum`: when true, the schema has an `enum` array — enum ALWAYS wins
 * over jsonType for widget selection (SchemaForm's enum-precedence rule).
 * Pass hasEnum=true and the canonical kind will be 'select' regardless of
 * jsonType, matching SchemaForm's existing behaviour.
 *
 * `opts.format`: optional JSON Schema `format` string — drives the
 * string→textarea-by-format branch.  The caller inspects the raw prop and
 * passes `format` here; fromJsonType cannot see the raw prop itself.
 *
 * NOTE: the string→textarea distinction based on `format` CANNOT be decided
 * from JsonType alone (which is only 'string'). The caller in derived-ports.ts
 * inspects `prop.format` and passes it here (or resolves to 'textarea' before
 * calling).
 *
 * `opts.integer`: (optional) when true, carries the integer coercion flag.
 * Canonical kind stays 'number' for both integer and number — the integer flag
 * is advisory for the caller's coercion logic (parseInt vs parseFloat).
 */
export function fromJsonType(
	t: JsonType,
	opts: { hasEnum: boolean; format?: string }
): FieldKind {
	// Enum takes precedence over jsonType (SchemaForm rule).
	if (opts.hasEnum) return 'select';

	switch (t) {
		case 'string':
			// format='textarea'|'multi-line' → textarea widget, else text.
			return opts.format === 'textarea' || opts.format === 'multi-line' ? 'textarea' : 'text';
		case 'integer':
		case 'number':
			return 'number';
		case 'boolean':
			return 'bool';
		case 'array':
			return 'json';
		case 'object':
			return 'json';
		case 'unknown':
			return 'json';
		default: {
			const _exhaustive: never = t;
			throw new Error(`fromJsonType: unmapped JsonType "${_exhaustive}"`);
		}
	}
}

// ---------------------------------------------------------------------------
// Utility: derive the integer flag from a JsonType (advisory — kind stays
// 'number' in both cases; callers use this to pick parseInt vs parseFloat).
// ---------------------------------------------------------------------------

export function isIntegerJsonType(t: JsonType): boolean {
	return t === 'integer';
}
