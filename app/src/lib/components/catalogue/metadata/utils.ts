// Plain-TS helpers shared by the format-specific metadata renderers. Copied
// verbatim (logic-identical) from ArtifactCard.svelte so renderers don't reach
// back into the card.

export const pct = (n: number) => `${Math.round(n * 100)}%`;

// unix_mode (e.g. 33188) → symbolic perms ("rw-r--r--").
export function symbolicMode(mode: number): string {
	const p = mode & 0o777;
	const bit = (n: number, r: string) => ((p >> n) & 1 ? r : '-');
	return [
		bit(8, 'r'), bit(7, 'w'), bit(6, 'x'),
		bit(5, 'r'), bit(4, 'w'), bit(3, 'x'),
		bit(2, 'r'), bit(1, 'w'), bit(0, 'x')
	].join('');
}

export function formatBytes(bytes: number | null | undefined): string {
	if (bytes === null || bytes === undefined) return '—';
	if (bytes === 0) return '0 B';
	const units = ['B', 'KB', 'MB', 'GB', 'TB'];
	const i = Math.floor(Math.log(bytes) / Math.log(1024));
	return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}
