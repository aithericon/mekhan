// Pure presentation helpers shared across the Model Pool tab pages
// (/models/*). House style — kept DOM-free so they're trivially unit-testable
// (see grouping.ts / inference-audit.ts).

/** Tailwind text tone for a model-pool lifecycle / replica status string. */
export function statusTone(s: string): string {
	if (s === 'active' || s === 'loaded') return 'text-emerald-600 dark:text-emerald-400';
	if (s === 'failed') return 'text-red-600 dark:text-red-400';
	if (s === 'stopped' || s === 'unloaded') return 'text-muted-foreground';
	if (s === 'sleeping') return 'text-indigo-500 dark:text-indigo-400';
	return 'text-amber-600 dark:text-amber-400';
}

/** First 8 chars of a uuid — the operator-readable short id. */
export const shortId = (id: string): string => id.slice(0, 8);
