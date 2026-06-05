// Pure presentation helpers for the inference audit table. Kept out of the
// .svelte file so the status→badge mapping can be unit-tested without mounting
// the DOM (the codebase's house style — see model-pool.ts / grouping.ts).
import type { InferenceRequestLogRow } from '$lib/api/inference';
import type { BadgeVariant } from '$lib/components/ui/badge';

/** A short id for mono display: first 8 chars (uuids/ulids read fine truncated). */
export function shortId(id: string | null | undefined): string {
	if (!id) return '—';
	return id.length > 8 ? id.slice(0, 8) : id;
}

/** in / out → total token triple, formatted with thousands separators. */
export function fmtTokens(row: InferenceRequestLogRow): {
	prompt: string;
	completion: string;
	total: string;
} {
	const f = (n: number) => n.toLocaleString();
	return {
		prompt: f(row.prompt_tokens),
		completion: f(row.completion_tokens),
		total: f(row.total_tokens)
	};
}

/**
 * Map a ledger row's free-text `status` to a Badge variant.
 *   completed                       → success (green)
 *   unmetered                       → warning (amber)
 *   cancelled / *_error / failed    → destructive (red)
 *   anything else (e.g. in-flight)  → muted
 */
export function statusVariant(status: string): BadgeVariant {
	const s = status.toLowerCase();
	if (s === 'completed') return 'success';
	if (s === 'unmetered') return 'warning';
	if (s === 'cancelled' || s === 'failed' || s.endsWith('_error')) return 'destructive';
	return 'muted';
}
