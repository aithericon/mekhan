/**
 * Canonical frontend FieldKind vocabulary.
 *
 * Single source of truth for the 12 value-input kinds recognised by the
 * shared FieldWidget renderer. Adding or removing a kind here causes every
 * exhaustive adapter switch and the FieldWidget template to fail the build,
 * enforcing a coordinated update.
 *
 * Wire vocabularies (port FieldKind from schema.d.ts, TaskFieldKind from
 * hpi/types.ts) are kept as-is — adapters in ./adapters.ts map them here.
 * Do NOT write canonical kinds back to the wire; authoring setters must
 * continue to write wire values.
 */

export const FIELD_KINDS = [
	'text',
	'textarea',
	'number',
	'bool',
	'select',
	'radio',
	'range',
	'rating',
	'date',
	'file',
	'signature',
	'json'
] as const;

export type FieldKind = (typeof FIELD_KINDS)[number];
