/**
 * Shared number formatting for the Data browser tabs — lifted out of
 * EntriesTab so Entries / Servers / Analytics can't drift on byte units.
 */

export function formatBytes(bytes: number | null | undefined): string {
	if (bytes === null || bytes === undefined) return '—';
	if (bytes === 0) return '0 B';
	const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
	const i = Math.min(
		Math.floor(Math.log(Math.abs(bytes)) / Math.log(1024)),
		units.length - 1
	);
	return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

export function formatCount(n: number | null | undefined): string {
	if (n === null || n === undefined) return '—';
	return n.toLocaleString();
}
