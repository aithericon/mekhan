import { describe, it, expect } from 'vitest';
import { resolveStatus, TONES } from './status-registry';

describe('resolveStatus', () => {
	it('maps in-flight states to the info tone and pulses them', () => {
		const running = resolveStatus('workflow', 'running');
		expect(running.tone).toBe('info');
		expect(running.pulse).toBe(true);
		expect(running.style).toBe(TONES.info);

		// A process "active" reads the SAME tone as a workflow "running" — the
		// whole point of the registry is that differently-spelled in-flight
		// states render identically.
		expect(resolveStatus('process', 'active').tone).toBe('info');
		expect(resolveStatus('process', 'active').pulse).toBe(true);
	});

	it('maps terminal success/failure consistently across domains', () => {
		for (const domain of ['workflow', 'step', 'process', 'task'] as const) {
			expect(resolveStatus(domain, 'completed').tone).toBe('success');
			expect(resolveStatus(domain, 'failed').tone).toBe('danger');
		}
	});

	it('lets a shared word carry a different tone per domain', () => {
		// "pending" is a neutral waiting state for a step, but a warning for a
		// human task / a cluster lease.
		expect(resolveStatus('step', 'pending').tone).toBe('neutral');
		expect(resolveStatus('task', 'pending').tone).toBe('warn');
		expect(resolveStatus('lease', 'pending').tone).toBe('warn');
	});

	it('carries domain-specific labels', () => {
		expect(resolveStatus('task', 'failed').label).toBe('Rejected');
		expect(resolveStatus('task', 'completed').label).toBe('Completed');
	});

	it('is case-insensitive on the status key', () => {
		expect(resolveStatus('workflow', 'RUNNING').tone).toBe('info');
	});

	it('falls back to neutral with the raw label for an unknown status', () => {
		const r = resolveStatus('workflow', 'frobnicating');
		expect(r.tone).toBe('neutral');
		expect(r.label).toBe('frobnicating');
		expect(r.pulse).toBe(false);
	});

	it('handles null/undefined without throwing', () => {
		expect(resolveStatus('workflow', null).tone).toBe('neutral');
		expect(resolveStatus('workflow', undefined).label).toBe('—');
	});
});
