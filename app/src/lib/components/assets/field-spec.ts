/**
 * Bridge an asset type's `PortField` (wire 9-variant FieldKind) to the canonical
 * `FieldSpec` the shared `FieldWidget` consumes. Mirrors the resource form's
 * SchemaForm derivation but works directly off the `Vec<PortField>` schema so we
 * skip the JSON-Schema round-trip (docs/20 "reuse PortField wholesale").
 */
import type { PortField } from '$lib/api/assets';
import type { FieldSpec } from '$lib/fields/spec';
import { defaultValueForKind } from '$lib/fields/spec';
import { fromPortFieldKind } from '$lib/fields/adapters';

/** Build a canonical FieldSpec from a single asset-type `PortField`. */
export function specFromPortField(field: PortField): FieldSpec {
	const kind = fromPortFieldKind(field.kind);
	return {
		name: field.name,
		kind,
		label: field.label,
		description: field.description ?? undefined,
		required: field.required ?? false,
		options: field.options ?? undefined,
		accept: field.accept ?? undefined,
		schema: field.schema,
		// Integer hint isn't carried on PortField; numbers stay float by default.
		testid: `asset-field-${field.name}`
	};
}

/** Seed a fresh record from the type schema (one zero-value per field). */
export function emptyRecord(fields: PortField[]): Record<string, unknown> {
	const row: Record<string, unknown> = {};
	for (const f of fields) {
		row[f.name] = defaultValueForKind(fromPortFieldKind(f.kind));
	}
	return row;
}

/**
 * Coerce one widget value into the typed JSONB shape the server validates
 * (mirrors `ResourceEditModal.buildConfig`). Empty optional values are dropped
 * by returning `undefined`; the caller omits those keys from the row.
 */
export function coerceFieldValue(field: PortField, raw: unknown): unknown {
	const kind = fromPortFieldKind(field.kind);
	const required = field.required ?? false;

	// File fields carry a storage-path string (upload- or catalog-sourced).
	if (kind === 'file') {
		if (typeof raw === 'string' && raw !== '') return raw;
		return undefined;
	}

	if (kind === 'bool') {
		if (raw === true || raw === 'true') return true;
		if (raw === false || raw === 'false') return false;
		return undefined;
	}

	if (kind === 'number') {
		if (raw === '' || raw === null || raw === undefined) return undefined;
		const n = typeof raw === 'number' ? raw : parseFloat(String(raw));
		return Number.isFinite(n) ? n : undefined;
	}

	if (kind === 'json') {
		if (typeof raw !== 'string' || raw.trim() === '') return undefined;
		try {
			return JSON.parse(raw);
		} catch {
			// Leave it as a string; server-side validation will surface the error.
			return raw;
		}
	}

	// text / textarea / select / radio / range / rating / date / signature
	const s = raw == null ? '' : String(raw);
	if (s === '' && !required) return undefined;
	return s;
}

/**
 * Build a validated record object from per-field widget values. Optional empty
 * fields are omitted (a missing field reads as absent/null on the server).
 */
export function buildRecord(
	fields: PortField[],
	values: Record<string, unknown>
): Record<string, unknown> {
	const out: Record<string, unknown> = {};
	for (const f of fields) {
		const v = coerceFieldValue(f, values[f.name]);
		if (v !== undefined) out[f.name] = v;
	}
	return out;
}

/** Display a record cell value compactly for the grid. */
export function displayCell(_field: PortField, value: unknown): string {
	if (value === undefined || value === null) return '';
	if (typeof value === 'object') return JSON.stringify(value);
	return String(value);
}
