import { CalendarDate } from '@internationalized/date';
import type { TaskField, TaskStep } from '@aithericon/hpi-ui/types';

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
		!field.options.includes(trimmed)
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

export function fieldsForStep(step: TaskStep): TaskField[] {
	const fields: TaskField[] = [];
	for (const block of step.blocks) {
		if (block.type === 'input') fields.push(block.field);
	}
	return fields;
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
