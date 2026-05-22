/**
 * Pure token / guard / duration helpers for the Petri inspector.
 *
 * Extracted verbatim from `Inspector.svelte` so the view stays declarative
 * and these predicates can be unit-tested. No DOM, no reactivity.
 */

/** A token-color-bearing value (the inspector only inspects `.color`). */
type ColoredToken = { color: { type: string; value?: unknown } };

export interface CoordinationProvenance {
	source: string;
	signal_type: string;
	workflow_id: string;
	adapter_pool: string;
	request_sent_at?: string;
	response_received_at?: string;
	confirm_sent_at?: string;
	transition?: string;
}

/** A "lease" token carries both a job id and a worker id in its data. */
export function isLeaseToken(token: ColoredToken): boolean {
	if (token.color.type !== 'Data' || !token.color.value) return false;
	const data = token.color.value as Record<string, unknown>;
	return 'job_id' in data && 'worker_id' in data;
}

export function hasCoordinationProvenance(token: ColoredToken): boolean {
	if (token.color.type !== 'Data' || !token.color.value) return false;
	const data = token.color.value as Record<string, unknown>;
	return '_provenance' in data && typeof data._provenance === 'object';
}

export function getCoordinationProvenance(
	token: ColoredToken
): CoordinationProvenance | null {
	if (!hasCoordinationProvenance(token)) return null;
	const data = token.color.value as Record<string, unknown>;
	return data._provenance as CoordinationProvenance;
}

export function getLeaseJobId(token: ColoredToken): string | null {
	if (!isLeaseToken(token)) return null;
	const data = token.color.value as Record<string, unknown>;
	return data.job_id as string;
}

/** Format a human-readable duration between two ISO timestamps. */
export function formatDuration(start: string, end: string): string {
	const startMs = new Date(start).getTime();
	const endMs = new Date(end).getTime();
	const durationMs = endMs - startMs;
	if (durationMs < 1000) return `${durationMs}ms`;
	return `${(durationMs / 1000).toFixed(2)}s`;
}

/** Render a transition guard descriptor into a label. */
export function formatGuard(
	guard:
		| {
				type: string;
				value?: unknown;
				field?: string;
				op?: string;
		  }
		| null
		| undefined
): string {
	if (!guard || guard.type === 'Always') return 'Always (no guard)';
	switch (guard.type) {
		case 'IntegerGreaterThan':
			return `Integer > ${guard.value}`;
		case 'IntegerLessThan':
			return `Integer < ${guard.value}`;
		case 'DataHasField':
			return `Has field "${guard.field}"`;
		case 'FieldCompare':
			return `${guard.field} ${guard.op} ${JSON.stringify(guard.value)}`;
		case 'ColorEquals':
			return `Color equals ${JSON.stringify(guard.value)}`;
		default:
			return guard.type;
	}
}

export function formatJson(value: unknown): string {
	return JSON.stringify(value, null, 2);
}
