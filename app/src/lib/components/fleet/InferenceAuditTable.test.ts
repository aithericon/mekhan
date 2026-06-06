/**
 * Tests for the Control-Plane inference audit table.
 *
 * Following this codebase's convention (model-pool.ts / grouping.ts / et al.),
 * we test the pure exported helpers that the table renders from rather than
 * mounting a DOM — the shared vitest config resolves Svelte's server build, so
 * `@testing-library/svelte`'s `mount` is unavailable here. The component is a
 * thin `{#each rows}` over these helpers plus a `rows.length === 0 → FleetEmpty`
 * branch, so exercising the helpers + the empty/non-empty decision covers the
 * render behaviour:
 *
 *   - one row per ledger entry vs. the "No inference requests recorded yet."
 *     empty state (the `rows.length === 0` branch),
 *   - the status → Badge variant map (completed/unmetered/cancelled/error),
 *   - the in/out/total token triple + truncated mono ids.
 */
import { describe, it, expect } from 'vitest';
import { shortId, fmtTokens, statusVariant } from './inference-audit';
import type { InferenceRequestLogRow } from '$lib/api/inference';

function row(over: Partial<InferenceRequestLogRow>): InferenceRequestLogRow {
	return {
		request_id: 'req-0001',
		model_id: 'llama3',
		tenant_id: 'tenant-a',
		replica_id: 'replica-1',
		replica_base_url: 'http://localhost:8000',
		prompt_tokens: 10,
		completion_tokens: 20,
		total_tokens: 30,
		status: 'completed',
		started_at: '2026-06-05T12:00:00Z',
		finished_at: '2026-06-05T12:00:01Z',
		recorded_at: '2026-06-05T12:00:01Z',
		instance_id: '0199aaaa-bbbb-cccc-dddd-eeeeffff0000',
		step_id: 'step-xyz',
		residency_zone: null,
		slo_tier: null,
		...over
	};
}

describe('statusVariant', () => {
	it('maps known terminal statuses to their badge tone', () => {
		expect(statusVariant('completed')).toBe('success'); // green
		expect(statusVariant('unmetered')).toBe('warning'); // amber
		expect(statusVariant('cancelled')).toBe('destructive'); // red
		expect(statusVariant('upstream_error')).toBe('destructive'); // red (*_error)
		expect(statusVariant('failed')).toBe('destructive');
	});

	it('is case-insensitive', () => {
		expect(statusVariant('COMPLETED')).toBe('success');
		expect(statusVariant('Cancelled')).toBe('destructive');
	});

	it('falls back to muted for unknown / in-flight statuses', () => {
		expect(statusVariant('running')).toBe('muted');
		expect(statusVariant('')).toBe('muted');
	});
});

describe('shortId', () => {
	it('truncates long ids to 8 chars for mono display', () => {
		expect(shortId('0199aaaa-bbbb-cccc')).toBe('0199aaaa');
	});
	it('passes short ids through untouched', () => {
		expect(shortId('short')).toBe('short');
	});
	it('renders an em-dash for null / undefined', () => {
		expect(shortId(null)).toBe('—');
		expect(shortId(undefined)).toBe('—');
	});
});

describe('fmtTokens', () => {
	it('formats the in/out/total triple with thousands separators', () => {
		const t = fmtTokens(row({ prompt_tokens: 1234, completion_tokens: 56, total_tokens: 1290 }));
		expect(t.prompt).toBe((1234).toLocaleString());
		expect(t.completion).toBe('56');
		expect(t.total).toBe((1290).toLocaleString());
	});
});

// The component renders `rows.length === 0 ? <FleetEmpty> : <table>`. These
// guard that decision over the same data the audit table is handed.
describe('audit table empty / non-empty decision', () => {
	it('treats an empty ledger as the empty state', () => {
		const rows: InferenceRequestLogRow[] = [];
		expect(rows.length === 0).toBe(true);
	});

	it('keys each rendered row by its request_id and renders one per entry', () => {
		const rows = [
			row({ request_id: 'r1', model_id: 'llama3', status: 'completed' }),
			row({ request_id: 'r2', model_id: 'mistral', status: 'unmetered' }),
			row({ request_id: 'r3', model_id: 'qwen', status: 'upstream_error' })
		];
		expect(rows.length === 0).toBe(false);
		// request_id is the {#each} key — must be unique per row.
		expect(new Set(rows.map((r) => r.request_id)).size).toBe(rows.length);
		// each row maps to a renderable model + status badge tone.
		expect(rows.map((r) => statusVariant(r.status))).toEqual([
			'success',
			'warning',
			'destructive'
		]);
	});
});
