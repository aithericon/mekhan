// Pure presentation helpers that turn a runner's live facets (host fingerprint,
// capabilities, resident model engines) into the identity a roster row shows —
// so a runner reads as a real machine ("Metal · 48 GB · gpu-box · 10.0.0.7",
// role "model server", serving llama3.2:1b) instead of an anonymous short id.
//
// DOM-free (house style — see model-pool.ts / grouping.ts) so it's trivially
// unit-tested. The roster (RunnerList) and the Engines lens (/models/engines)
// both import these, so the two surfaces can't drift in how they describe a host.
import type { RunnerPresenceSnapshot } from '$lib/api/runners';

export type Host = NonNullable<RunnerPresenceSnapshot['host']>;

/** Compact accelerator label, e.g. "CUDA ×2 · 80 GB · cc9.0",
 *  "Metal · 48 GB unified", or "CPU". */
export function accelLabel(h: Host): string {
	const accel = (h.accelerator ?? '').toLowerCase();
	if (accel === 'cuda' || accel === 'rocm') {
		let head = accel.toUpperCase();
		if (h.gpu_count) head += ` ×${h.gpu_count}`;
		const parts = [head];
		if (h.vram_gb) parts.push(`${h.vram_gb} GB`);
		if (h.compute_capability) parts.push(`cc${h.compute_capability}`);
		return parts.join(' · ');
	}
	if (accel === 'metal') return `Metal${h.vram_gb ? ` · ${h.vram_gb} GB unified` : ''}`;
	if (accel === 'cpu') return 'CPU';
	return h.accelerator ?? '—';
}

/** One-line host summary for a roster row, e.g. "CUDA ×2 · 80 GB · gpu-box · 10.0.0.7".
 *  Empty string when the runner reports no host (legacy binary without the probe). */
export function hostSummary(h: Host | null | undefined): string {
	if (!h) return '';
	const parts: string[] = [];
	const accel = (h.accelerator ?? '').toLowerCase();
	if (accel === 'cuda' || accel === 'rocm') {
		let head = accel.toUpperCase();
		if (h.gpu_count) head += ` ×${h.gpu_count}`;
		if (h.vram_gb) head += ` · ${h.vram_gb} GB`;
		parts.push(head);
	} else if (accel === 'metal') {
		parts.push(`Metal${h.vram_gb ? ` · ${h.vram_gb} GB` : ''}`);
	} else if (accel === 'cpu') {
		parts.push(h.cpu_cores ? `CPU · ${h.cpu_cores} cores` : 'CPU');
	}
	if (h.hostname) parts.push(h.hostname);
	if (h.ips && h.ips.length > 0) parts.push(h.ips[0]);
	return parts.join(' · ');
}

export type RunnerRoleKey = 'model' | 'robot' | 'instrument' | 'generic';

export interface RunnerRole {
	key: RunnerRoleKey;
	/** Operator-readable role label for the roster badge. */
	label: string;
}

/**
 * Classify a runner's PRIMARY role from its capabilities + whether it serves
 * model engines. This is what gives an otherwise-ungrouped runner (a model
 * server dispatches inference over HTTP, so it correctly carries no presence
 * group) a first-class identity in the roster.
 *
 * `model` wins when the runner serves engines: a node may both serve models and
 * advertise other caps, and "model server" is the most operator-salient fact.
 */
export function runnerRole(
	caps: Record<string, unknown> | null | undefined,
	hasEngines: boolean
): RunnerRole {
	if (hasEngines) return { key: 'model', label: 'model server' };
	const keys = caps && typeof caps === 'object' ? Object.keys(caps) : [];
	if (keys.includes('ros')) return { key: 'robot', label: 'robot' };
	if (keys.includes('xrd')) return { key: 'instrument', label: 'instrument' };
	return { key: 'generic', label: 'runner' };
}

/**
 * A runner is "stale" when it is online (heartbeating) yet reports no host
 * fingerprint — i.e. it runs an executor binary built before the host probe.
 * Surfaced as a hint so an operator knows to restart it to de-anonymise, not as
 * an error (it's still serving). Offline runners are not stale, just offline.
 */
export function isStale(snap: RunnerPresenceSnapshot | undefined): boolean {
	return !!snap?.present && !snap.host;
}
