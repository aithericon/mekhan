import { CalendarDate } from '@internationalized/date';
import type { TaskField, TaskStep } from '$lib/hpi/types';

export type UploadedFile = { url: string; name: string; size: number; type: string };

// ── Value helpers ──────────────────────────────────────────────

export function getTextValue(formData: Record<string, unknown>, name: string): string {
	const value = formData[name];
	if (typeof value === 'number') return Number.isFinite(value) ? String(value) : '';
	return typeof value === 'string' ? value : '';
}

export function getCheckboxValue(formData: Record<string, unknown>, name: string): boolean {
	return formData[name] === true;
}

export function getNumberValue(formData: Record<string, unknown>, name: string): number {
	const value = formData[name];
	if (typeof value === 'number') return value;
	if (typeof value === 'string' && value.length > 0) {
		const n = Number(value);
		if (Number.isFinite(n)) return n;
	}
	return 0;
}

// ── Setters (use with formData.update) ─────────────────────────

export function makeSetTextValue(
	formData: { update: (fn: (c: Record<string, unknown>) => Record<string, unknown>) => void },
	clearError: (name: string) => void
) {
	return (name: string, value: string) => {
		formData.update((current) => ({ ...current, [name]: value }));
		clearError(name);
	};
}

export function makeSetCheckboxValue(
	formData: { update: (fn: (c: Record<string, unknown>) => Record<string, unknown>) => void },
	clearError: (name: string) => void
) {
	return (name: string, value: boolean) => {
		formData.update((current) => ({ ...current, [name]: value }));
		clearError(name);
	};
}

export function makeSetNumberValue(
	formData: { update: (fn: (c: Record<string, unknown>) => Record<string, unknown>) => void },
	clearError: (name: string) => void
) {
	return (name: string, value: number) => {
		formData.update((current) => ({ ...current, [name]: String(value) }));
		clearError(name);
	};
}

// ── Date helpers ───────────────────────────────────────────────

/** Parse a stored YYYY-MM-DD or YYYY-MM-DDTHH:MM string into a CalendarDate */
export function parseCalendarDate(str: string): CalendarDate | undefined {
	const datePart = str.split('T')[0];
	const m = datePart?.match(/^(\d{4})-(\d{2})-(\d{2})$/);
	if (!m) return undefined;
	return new CalendarDate(Number(m[1]), Number(m[2]), Number(m[3]));
}

/** Extract HH:MM from a stored YYYY-MM-DDTHH:MM string, or return '' */
export function parseTimePart(str: string): string {
	const idx = str.indexOf('T');
	return idx >= 0 ? str.slice(idx + 1) : '';
}

/** Build the stored string from a CalendarDate + optional time */
export function buildDateString(date: CalendarDate | undefined, time: string): string {
	if (!date) return '';
	const d = `${String(date.year).padStart(4, '0')}-${String(date.month).padStart(2, '0')}-${String(date.day).padStart(2, '0')}`;
	return time ? `${d}T${time}` : d;
}

// ── File helpers ───────────────────────────────────────────────

export function parseFileValue(value: string): UploadedFile[] {
	if (!value) return [];
	try {
		return JSON.parse(value);
	} catch {
		return [];
	}
}

// ── Error helpers ──────────────────────────────────────────────

export function makeClearFieldError(formErrors: {
	update: (fn: (c: Record<string, unknown>) => Record<string, unknown>) => void;
}) {
	return (name: string) => {
		formErrors.update((current) => {
			const next = { ...current };
			delete next[name];
			return next;
		});
	};
}

export function makeSetFieldError(formErrors: {
	update: (fn: (c: Record<string, unknown>) => Record<string, unknown>) => void;
}) {
	return (name: string, message: string) => {
		formErrors.update((current) => ({ ...current, [name]: [message] }));
	};
}

// ── Validation ─────────────────────────────────────────────────

export function validateField(field: TaskField, formData: Record<string, unknown>): string | null {
	const raw = formData[field.name];

	if (field.kind === 'checkbox') {
		if (field.required && raw !== true) return `${field.label} must be checked`;
		return null;
	}

	if (field.kind === 'signature') {
		const sigStr = typeof raw === 'string' ? raw : '';
		if (field.required) {
			try {
				const parsed = JSON.parse(sigStr);
				if (!parsed?.data) return `${field.label} is required`;
			} catch {
				return `${field.label} is required`;
			}
		}
		return null;
	}

	const textValue = typeof raw === 'number' ? String(raw) : typeof raw === 'string' ? raw : '';
	const trimmed = textValue.trim();

	if (field.required && trimmed.length === 0) return `${field.label} is required`;
	if (field.kind === 'number' && trimmed.length > 0 && !Number.isFinite(Number(trimmed)))
		return `${field.label} must be a valid number`;
	if (
		(field.kind === 'select' || field.kind === 'radio') &&
		field.options?.length &&
		trimmed.length > 0 &&
		!field.options.some((o) => o.value === trimmed)
	)
		return `Select a valid value for ${field.label}`;
	if (
		field.kind === 'date' &&
		trimmed.length > 0 &&
		!/^\d{4}-\d{2}-\d{2}(T\d{2}:\d{2})?$/.test(trimmed)
	)
		return `${field.label} must be a valid date`;
	if ((field.kind === 'range' || field.kind === 'rating') && trimmed.length > 0) {
		const num = Number(trimmed);
		if (!Number.isFinite(num)) return `${field.label} must be a valid number`;
	}

	return null;
}

/**
 * Coerce validated form values to the JSON shape their declared
 * `TaskFieldKind` implies, just before submit. Numbers/sliders/ratings are
 * kept as strings in the reactive store (see `makeSetNumberValue`) so the
 * inputs stay controlled, but the compiler's enforced `Data__*` schema types
 * them strictly — the wire payload must carry a real `number`/`boolean` or
 * the net wedges at `t_*_yield`. `validateFields` has already run, so every
 * numeric value here is finite; bad input never reaches this and the user has
 * already seen a per-field error. A blank optional numeric value is dropped
 * (an unfilled number is "not provided"; the open-`additionalProperties`
 * schema accepts its absence — a present `""` would not).
 */
export function coerceFormData(
	fields: TaskField[],
	formData: Record<string, unknown>
): Record<string, unknown> {
	const out: Record<string, unknown> = { ...formData };
	for (const field of fields) {
		const raw = out[field.name];
		if (field.kind === 'number' || field.kind === 'range' || field.kind === 'rating') {
			if (typeof raw === 'number') continue;
			const trimmed = typeof raw === 'string' ? raw.trim() : '';
			if (trimmed.length === 0) {
				delete out[field.name];
			} else {
				const n = Number(trimmed);
				if (Number.isFinite(n)) out[field.name] = n;
			}
		} else if (field.kind === 'checkbox') {
			out[field.name] = raw === true;
		}
	}
	return out;
}

export function fieldsForStep(step: TaskStep): TaskField[] {
	const fields: TaskField[] = [];
	for (const block of step.blocks) {
		if (block.type === 'input') fields.push(block.field);
	}
	return fields;
}

// ── Repeater helpers (Feature B) ───────────────────────────────────

/**
 * Parse a Repeater `items_ref` / `item_label_ref` of the form
 * `<head>.<seg>[.<seg>...]+[*]+[.<seg>...]*`. Mirrors the compiler's
 * `parse_repeater_ref` (service/src/compiler/validate.rs) — returns
 * `null` for malformed inputs so the renderer can degrade gracefully
 * (an empty Repeater + a row count of 0).
 */
export function parseRepeaterRef(
	raw: string
): { head: string; pre: string[]; post: string[] } | null {
	const trimmed = raw.trim();
	if (!trimmed) return null;
	const first = trimmed.indexOf('[*]');
	if (first < 0) return null;
	if (trimmed.slice(first + 3).includes('[*]')) return null;
	const before = trimmed.slice(0, first);
	const afterRaw = trimmed.slice(first + 3);
	const after = afterRaw.startsWith('.') ? afterRaw.slice(1) : afterRaw;
	const dot = before.indexOf('.');
	if (dot < 0) return null;
	const head = before.slice(0, dot);
	if (!head) return null;
	const preStr = before.slice(dot + 1);
	if (!preStr) return null;
	const pre = preStr.split('.');
	if (pre.some((s) => s.length === 0)) return null;
	const post = after.length === 0 ? [] : after.split('.');
	if (post.some((s) => s.length === 0)) return null;
	return { head, pre, post };
}

/**
 * Walk `data` following `path` (dotted segments) and return whatever
 * sits there, or undefined if any hop is missing / non-objectish.
 * Used by the Repeater renderer to resolve the upstream array AND
 * per-element label values.
 */
export function getAtPath(data: unknown, path: string[]): unknown {
	let cur: unknown = data;
	for (const seg of path) {
		if (cur == null || typeof cur !== 'object') return undefined;
		cur = (cur as Record<string, unknown>)[seg];
	}
	return cur;
}

/** Coerce the resolved items value to a plain array. */
export function asItemsArray(value: unknown): unknown[] {
	if (Array.isArray(value)) return value;
	return [];
}

/**
 * Substitute every `{{ <head>.<...pre>[*].<...rest> }}` placeholder in
 * `source` whose `head + pre` matches the parsed Repeater ref with the
 * value at `getAtPath(item, rest)` (stringified). Non-matching
 * placeholders pass through unchanged so the task-level staging can
 * still resolve them. Used by the Repeater renderer to scope display
 * blocks (Mdsvex, Callout, Image, …) to the current row.
 */
export function interpolateRowPlaceholders(
	source: string,
	parsed: { head: string; pre: string[] },
	item: unknown
): string {
	if (!source.includes('{{')) return source;
	const expectedPrefix = [parsed.head, ...parsed.pre].join('.');
	return source.replace(/\{\{\s*([^}]+?)\s*\}\}/g, (full, raw: string) => {
		const ref = raw.trim();
		const star = ref.indexOf('[*]');
		if (star < 0) return full;
		const before = ref.slice(0, star);
		if (before !== expectedPrefix) return full;
		const afterRaw = ref.slice(star + 3);
		const after = afterRaw.startsWith('.') ? afterRaw.slice(1) : afterRaw;
		const rest = after.length === 0 ? [] : after.split('.');
		const value = rest.length === 0 ? item : getAtPath(item, rest);
		if (value == null) return '';
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean') return String(value);
		return JSON.stringify(value);
	});
}

export function validateFields(
	fields: TaskField[],
	formData: Record<string, unknown>,
	setError: (name: string, message: string) => void,
	clearError: (name: string) => void
): string | null {
	let firstInvalid: string | null = null;
	for (const field of fields) {
		const message = validateField(field, formData);
		if (message) {
			setError(field.name, message);
			if (!firstInvalid) firstInvalid = field.name;
		} else {
			clearError(field.name);
		}
	}
	return firstInvalid;
}

// ── Misc ───────────────────────────────────────────────────────

export function plainDescription(mdsvex?: string): string {
	if (!mdsvex) return '';
	return mdsvex
		.replace(/```[\s\S]*?```/g, '')
		.replace(/[#>*_`~\[\]()!-]/g, ' ')
		.replace(/\s+/g, ' ')
		.trim();
}
