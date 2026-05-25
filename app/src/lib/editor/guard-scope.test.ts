import { describe, it, expect } from 'vitest';
import { buildResourceScope } from './guard-scope';
import type { ResourceTypeInfo } from '$lib/api/resources';

const postgres: ResourceTypeInfo = {
	name: 'postgres',
	display_name: 'Postgres',
	icon: 'lucide-database',
	public_fields: ['host', 'port', 'database', 'username'],
	secret_fields: ['password'],
	schema: {}
};

const openai: ResourceTypeInfo = {
	name: 'openai',
	display_name: 'OpenAI',
	icon: 'lucide-sparkles',
	public_fields: ['base_url'],
	secret_fields: ['api_key'],
	schema: {}
};

describe('buildResourceScope', () => {
	it('returns [] when no resources are declared', () => {
		expect(buildResourceScope(undefined, [postgres])).toEqual([]);
		expect(buildResourceScope({}, [postgres])).toEqual([]);
	});

	it('emits one entry per (alias, field) pair, public then secret', () => {
		const out = buildResourceScope({ db: 'postgres' }, [postgres]);
		expect(out.map((e) => e.qualified)).toEqual([
			'db.host',
			'db.port',
			'db.database',
			'db.username',
			'db.password'
		]);
		// Each entry uses the alias as nodeLabel + synthetic resource: id.
		expect(out.every((e) => e.nodeLabel === 'db')).toBe(true);
		expect(out.every((e) => e.nodeId === 'resource:db')).toBe(true);
	});

	it('aliases are alphabetised to mirror the BTreeMap ordering on the wire', () => {
		const out = buildResourceScope({ zeta: 'openai', alpha: 'openai' }, [openai]);
		// alpha's entries come first, then zeta's.
		expect(out.map((e) => e.qualified)).toEqual([
			'alpha.base_url',
			'alpha.api_key',
			'zeta.base_url',
			'zeta.api_key'
		]);
	});

	it('typed `port` field gets the number kind; everything else is text', () => {
		const out = buildResourceScope({ db: 'postgres' }, [postgres]);
		expect(out.find((e) => e.field === 'port')?.kind).toBe('number');
		expect(out.find((e) => e.field === 'host')?.kind).toBe('text');
		expect(out.find((e) => e.field === 'password')?.kind).toBe('text');
	});

	it('aliases whose type is not in the registry are dropped silently', () => {
		// `slack` isn't in the types list — the entire alias is omitted so the
		// picker doesn't render a stub. Other aliases are unaffected.
		const out = buildResourceScope({ db: 'postgres', notify: 'slack' }, [postgres]);
		expect(out.every((e) => e.nodeLabel === 'db')).toBe(true);
		expect(out.length).toBe(5);
	});
});
