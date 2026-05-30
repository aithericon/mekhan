import { describe, it, expect } from 'vitest';
import { deriveFieldSpecs, discriminatorOf } from './SchemaForm.svelte';

// Mirrors the schemars output for the `datacenter` discriminated resource:
// a `oneOf` of variants each tagged by a single-value `scheduler_flavor` enum.
const DC_SCHEMA = {
	oneOf: [
		{
			properties: {
				scheduler_flavor: { enum: ['slurm'], type: 'string' },
				ssh_host: { type: 'string' },
				ssh_key: { type: 'string' }
			},
			required: ['scheduler_flavor', 'ssh_host', 'ssh_key']
		},
		{
			properties: {
				scheduler_flavor: { enum: ['nomad'], type: 'string' },
				nomad_addr: { type: 'string' }
			},
			required: ['scheduler_flavor', 'nomad_addr']
		}
	]
};

describe('deriveFieldSpecs — discriminated (oneOf) schema', () => {
	it('detects the const-tag discriminator', () => {
		expect(discriminatorOf(DC_SCHEMA)).toBe('scheduler_flavor');
		expect(discriminatorOf({ properties: { a: { type: 'string' } } })).toBeNull();
	});

	it('with no flavor chosen, renders only the discriminator select', () => {
		const specs = deriveFieldSpecs(DC_SCHEMA, [], undefined, undefined);
		expect(specs.map((s) => s.name)).toEqual(['scheduler_flavor']);
		expect(specs[0].enumOptions).toEqual(['slurm', 'nomad']);
		expect(specs[0].isRequired).toBe(true);
	});

	it('with slurm chosen, renders ONLY the slurm variant fields', () => {
		const specs = deriveFieldSpecs(DC_SCHEMA, ['ssh_key'], ['ssh_host', 'ssh_key'], 'slurm');
		expect(specs.map((s) => s.name)).toEqual(['scheduler_flavor', 'ssh_host', 'ssh_key']);
		expect(specs.find((s) => s.name === 'ssh_key')?.isSecret).toBe(true);
		expect(specs.find((s) => s.name === 'ssh_host')?.isRequired).toBe(true);
		// The nomad field never leaks into a slurm datacenter.
		expect(specs.find((s) => s.name === 'nomad_addr')).toBeUndefined();
	});

	it('switching to nomad swaps the variant fields', () => {
		const specs = deriveFieldSpecs(DC_SCHEMA, [], undefined, 'nomad');
		const names = specs.map((s) => s.name);
		expect(names).toContain('nomad_addr');
		expect(names).not.toContain('ssh_host');
	});

	it('plain object schema is unaffected', () => {
		const flat = {
			properties: { host: { type: 'string' }, port: { type: 'integer' } },
			required: ['host']
		};
		const specs = deriveFieldSpecs(flat, [], ['host', 'port']);
		expect(specs.map((s) => s.name)).toEqual(['host', 'port']);
		expect(specs.find((s) => s.name === 'host')?.isRequired).toBe(true);
	});
});
