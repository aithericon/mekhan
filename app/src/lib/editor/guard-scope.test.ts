import { describe, it, expect } from 'vitest';
import {
	buildResourceScope,
	tyDescriptorLabel,
	tyDescriptorToFieldKind,
	type TyDescriptor
} from './guard-scope';
import type { ResourceSummary, ResourceTypeInfo } from '$lib/api/resources';

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

function resource(
	path: string,
	resource_type: string,
	overrides: Partial<ResourceSummary> = {}
): ResourceSummary {
	return {
		id: `id-${path}`,
		path,
		display_name: path,
		resource_type,
		latest_version: 1,
		created_at: '2026-01-01T00:00:00Z',
		updated_at: '2026-01-01T00:00:00Z',
		...overrides
	};
}

describe('buildResourceScope', () => {
	it('returns [] when no resources exist', () => {
		expect(buildResourceScope(undefined, [postgres])).toEqual([]);
		expect(buildResourceScope([], [postgres])).toEqual([]);
	});

	it('emits one entry per (resource, field) pair, public then secret', () => {
		const out = buildResourceScope([resource('local_pg', 'postgres')], [postgres]);
		expect(out.map((e) => e.qualified)).toEqual([
			'local_pg.host',
			'local_pg.port',
			'local_pg.database',
			'local_pg.username',
			'local_pg.password'
		]);
		// Each entry uses the resource's path as nodeLabel + synthetic resource:<id>.
		expect(out.every((e) => e.nodeLabel === 'local_pg')).toBe(true);
		expect(out.every((e) => e.nodeId === 'resource:id-local_pg')).toBe(true);
	});

	it('resources are alphabetised by path for stable picker order', () => {
		const out = buildResourceScope(
			[resource('zeta_ai', 'openai'), resource('alpha_ai', 'openai')],
			[openai]
		);
		// alpha_ai's entries come first, then zeta_ai's — matches what the
		// user types when authoring `<path>.<field>`.
		expect(out.map((e) => e.qualified)).toEqual([
			'alpha_ai.base_url',
			'alpha_ai.api_key',
			'zeta_ai.base_url',
			'zeta_ai.api_key'
		]);
	});

	it('typed `port` field gets the number kind; everything else is text', () => {
		const out = buildResourceScope([resource('local_pg', 'postgres')], [postgres]);
		expect(out.find((e) => e.field === 'port')?.kind).toBe('number');
		expect(out.find((e) => e.field === 'host')?.kind).toBe('text');
		expect(out.find((e) => e.field === 'password')?.kind).toBe('text');
	});

	it('resources whose type is not in the registry are dropped silently', () => {
		// `slack` isn't in the types list — the entire resource is omitted so
		// the picker doesn't render unconsumable fields.
		const out = buildResourceScope(
			[resource('local_pg', 'postgres'), resource('notify', 'slack')],
			[postgres]
		);
		expect(out.every((e) => e.nodeLabel === 'local_pg')).toBe(true);
		expect(out.length).toBe(5);
	});

	it('display_name overrides path as the picker label', () => {
		const out = buildResourceScope(
			[resource('f/team/local_pg', 'postgres', { display_name: 'Local Postgres' })],
			[postgres]
		);
		// Label shows the human-friendly name…
		expect(out[0].nodeLabel).toBe('Local Postgres');
		// …but qualified ref uses the path (what the compiler matches).
		expect(out[0].qualified).toBe('f/team/local_pg.host');
	});
});

describe('TyDescriptor helpers', () => {
	const scalarString: TyDescriptor = { kind: 'scalar', name: 'String' };
	const scalarNumber: TyDescriptor = { kind: 'scalar', name: 'Number' };
	const fileEnvelope: TyDescriptor = {
		kind: 'object',
		selectable: true,
		fields: {
			url: scalarString,
			filename: scalarString,
			content_type: scalarString
		}
	};
	const taskArray: TyDescriptor = {
		kind: 'array',
		element: {
			kind: 'object',
			selectable: false,
			fields: { title: scalarString, priority: scalarString }
		}
	};

	it('tyDescriptorLabel renders scalar names verbatim', () => {
		expect(tyDescriptorLabel(scalarString)).toBe('String');
		expect(tyDescriptorLabel(scalarNumber)).toBe('Number');
	});

	it('tyDescriptorLabel renders array<element>', () => {
		expect(tyDescriptorLabel(taskArray)).toBe('array<{title, priority}>');
	});

	it('tyDescriptorLabel renders compact object form {a, b, …}', () => {
		expect(tyDescriptorLabel(fileEnvelope)).toBe('{url, filename, content_type}');
		const big: TyDescriptor = {
			kind: 'object',
			selectable: false,
			fields: { a: scalarString, b: scalarString, c: scalarString, d: scalarString }
		};
		expect(tyDescriptorLabel(big)).toBe('{a, b, c, …}');
	});

	it('tyDescriptorLabel handles any / opaque / undefined', () => {
		expect(tyDescriptorLabel({ kind: 'any' })).toBe('any');
		expect(tyDescriptorLabel({ kind: 'opaque', name: 'Detail' })).toBe('Opaque(Detail)');
		expect(tyDescriptorLabel(undefined)).toBe('unknown');
	});

	it('tyDescriptorToFieldKind maps scalars; collapses non-scalars to json', () => {
		expect(tyDescriptorToFieldKind(scalarString)).toBe('text');
		expect(tyDescriptorToFieldKind(scalarNumber)).toBe('number');
		expect(tyDescriptorToFieldKind({ kind: 'scalar', name: 'Bool' })).toBe('bool');
		expect(tyDescriptorToFieldKind({ kind: 'scalar', name: 'FileRef' })).toBe('file');
		expect(tyDescriptorToFieldKind({ kind: 'scalar', name: 'Timestamp' })).toBe('timestamp');
		expect(tyDescriptorToFieldKind(fileEnvelope)).toBe('json');
		expect(tyDescriptorToFieldKind(taskArray)).toBe('json');
		expect(tyDescriptorToFieldKind(undefined)).toBe('json');
	});
});
