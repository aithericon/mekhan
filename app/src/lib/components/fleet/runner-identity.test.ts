import { describe, it, expect } from 'vitest';
import { accelLabel, hostSummary, runnerRole, isStale, type Host } from './runner-identity';
import type { RunnerPresenceSnapshot } from '$lib/api/runners';

const cuda: Host = { accelerator: 'cuda', gpu_count: 2, vram_gb: 80, compute_capability: '9.0' };
const metal: Host = { accelerator: 'metal', vram_gb: 48, hostname: 'mac', ips: ['192.168.0.1'] };

describe('accelLabel', () => {
	it('renders CUDA with count, vram, and compute capability', () => {
		expect(accelLabel(cuda)).toBe('CUDA ×2 · 80 GB · cc9.0');
	});
	it('renders Metal with unified memory', () => {
		expect(accelLabel(metal)).toBe('Metal · 48 GB unified');
	});
	it('renders bare CPU', () => {
		expect(accelLabel({ accelerator: 'cpu' })).toBe('CPU');
	});
	it('falls back to an em-dash when accelerator is unknown', () => {
		expect(accelLabel({})).toBe('—');
	});
});

describe('hostSummary', () => {
	it('is empty when there is no host (legacy binary)', () => {
		expect(hostSummary(null)).toBe('');
		expect(hostSummary(undefined)).toBe('');
	});
	it('joins accelerator · hostname · first ip', () => {
		expect(hostSummary(metal)).toBe('Metal · 48 GB · mac · 192.168.0.1');
	});
	it('summarises a CUDA box', () => {
		expect(hostSummary({ accelerator: 'cuda', gpu_count: 2, vram_gb: 80, hostname: 'gpu-box', ips: ['10.0.0.7'] })).toBe(
			'CUDA ×2 · 80 GB · gpu-box · 10.0.0.7'
		);
	});
});

describe('runnerRole', () => {
	it('classifies a model server when it serves engines (wins over other caps)', () => {
		expect(runnerRole({ xrd: {} }, true)).toEqual({ key: 'model', label: 'model server' });
	});
	it('classifies a robot from a ros capability', () => {
		expect(runnerRole({ ros: { robot_model: 'xarm6' } }, false)).toEqual({ key: 'robot', label: 'robot' });
	});
	it('classifies an instrument from an xrd capability', () => {
		expect(runnerRole({ xrd: { max_2theta: 180 } }, false)).toEqual({ key: 'instrument', label: 'instrument' });
	});
	it('falls back to a generic runner', () => {
		expect(runnerRole({}, false)).toEqual({ key: 'generic', label: 'runner' });
		expect(runnerRole(null, false)).toEqual({ key: 'generic', label: 'runner' });
	});
});

describe('isStale', () => {
	const present = (host?: Host): RunnerPresenceSnapshot =>
		({ runner_id: 'r', present: true, last_seen_ms_ago: 100, backends: [], host }) as RunnerPresenceSnapshot;
	it('is stale when online but missing a host fingerprint', () => {
		expect(isStale(present())).toBe(true);
	});
	it('is not stale when online with a host', () => {
		expect(isStale(present(metal))).toBe(false);
	});
	it('is not stale when offline or unknown', () => {
		expect(isStale({ runner_id: 'r', present: false, last_seen_ms_ago: 1, backends: [] } as RunnerPresenceSnapshot)).toBe(false);
		expect(isStale(undefined)).toBe(false);
	});
});
