// Shared formatting helpers for the Fleet views. Previously copy-pasted into
// RunnerList, PresenceBoard, and WorkerPoolBoard — one home now so they can't
// drift.

/** "just now" / "3s ago" / "5m ago" / "2h ago" from a millisecond age. */
export function fmtMsAgo(ms: number): string {
	if (ms < 2000) return 'just now';
	if (ms < 60_000) return `${Math.round(ms / 1000)}s ago`;
	if (ms < 3_600_000) return `${Math.round(ms / 60_000)}m ago`;
	return `${Math.round(ms / 3_600_000)}h ago`;
}

/** A locale date string from an ISO timestamp; "—" for null/undefined, and the
    raw string back if it doesn't parse. */
export function fmtDate(ts: string | null | undefined): string {
	if (!ts) return '—';
	const d = new Date(ts);
	return Number.isNaN(d.getTime()) ? ts : d.toLocaleString();
}
