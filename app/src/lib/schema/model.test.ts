import { describe, it, expect } from 'vitest';
import {
	tyDescriptorToSchemaNode,
	jsonSchemaToSchemaNode,
	portToSchemaNode,
	fileMetadataDataTypeToSchemaNode,
	catalogueColumnsToSchemaNode,
	isPrimitive,
	isFileRef,
	isStorageKey
} from './model';
import type { TyDescriptor } from '$lib/editor/guard-scope';
import type { components } from '$lib/api/schema';

type Port = components['schemas']['Port'];

describe('tyDescriptorToSchemaNode', () => {
	it('converts a scalar TyDescriptor', () => {
		const ty: TyDescriptor = { kind: 'scalar', name: 'String' };
		const node = tyDescriptorToSchemaNode(ty);
		expect(node.kind).toBe('scalar');
		if (node.kind === 'scalar') {
			expect(node.name).toBe('String');
			expect(node.label).toBe('String');
		}
	});

	it('converts an array TyDescriptor', () => {
		const ty: TyDescriptor = { kind: 'array', element: { kind: 'scalar', name: 'Number' } };
		const node = tyDescriptorToSchemaNode(ty);
		expect(node.kind).toBe('array');
		if (node.kind === 'array') {
			expect(node.label).toBe('array<Number>');
			expect(node.element.kind).toBe('scalar');
			if (node.element.kind === 'scalar') {
				expect(node.element.name).toBe('Number');
			}
		}
	});

	it('converts an object TyDescriptor', () => {
		const ty: TyDescriptor = {
			kind: 'object',
			fields: {
				name: { kind: 'scalar', name: 'String' },
				age: { kind: 'scalar', name: 'Number' }
			},
			selectable: true
		};
		const node = tyDescriptorToSchemaNode(ty);
		expect(node.kind).toBe('object');
		if (node.kind === 'object') {
			expect(node.selectable).toBe(true);
			expect(node.fields.has('name')).toBe(true);
			expect(node.fields.has('age')).toBe(true);
			const nameNode = node.fields.get('name')!;
			expect(nameNode.kind).toBe('scalar');
		}
	});

	it('converts a nested object TyDescriptor', () => {
		const ty: TyDescriptor = {
			kind: 'object',
			fields: {
				address: {
					kind: 'object',
					fields: {
						street: { kind: 'scalar', name: 'String' },
						zip: { kind: 'scalar', name: 'String' }
					},
					selectable: false
				}
			},
			selectable: false
		};
		const node = tyDescriptorToSchemaNode(ty);
		expect(node.kind).toBe('object');
		if (node.kind === 'object') {
			const addrNode = node.fields.get('address')!;
			expect(addrNode.kind).toBe('object');
			if (addrNode.kind === 'object') {
				expect(addrNode.fields.has('street')).toBe(true);
				expect(addrNode.fields.has('zip')).toBe(true);
			}
		}
	});

	it('converts any TyDescriptor', () => {
		const ty: TyDescriptor = { kind: 'any' };
		const node = tyDescriptorToSchemaNode(ty);
		expect(node.kind).toBe('any');
		expect(node.label).toBe('any');
	});

	it('converts opaque TyDescriptor', () => {
		const ty: TyDescriptor = { kind: 'opaque', name: 'SomeType' };
		const node = tyDescriptorToSchemaNode(ty);
		expect(node.kind).toBe('opaque');
		if (node.kind === 'opaque') {
			expect(node.name).toBe('SomeType');
		}
	});

	it('handles undefined input as any', () => {
		const node = tyDescriptorToSchemaNode(undefined);
		expect(node.kind).toBe('any');
	});

	it('round-trips scalar label', () => {
		const ty: TyDescriptor = { kind: 'scalar', name: 'Bool' };
		const node = tyDescriptorToSchemaNode(ty);
		expect(node.label).toBe('Bool');
	});

	it('round-trips array<object> nested label', () => {
		const ty: TyDescriptor = {
			kind: 'array',
			element: {
				kind: 'object',
				fields: { x: { kind: 'scalar', name: 'Number' } },
				selectable: false
			}
		};
		const node = tyDescriptorToSchemaNode(ty);
		expect(node.label).toMatch(/array</);
		if (node.kind === 'array') {
			expect(node.element.kind).toBe('object');
		}
	});
});

describe('value heuristics', () => {
	it('isPrimitive: null/undefined/string/number/bool are primitive', () => {
		expect(isPrimitive(null)).toBe(true);
		expect(isPrimitive(undefined)).toBe(true);
		expect(isPrimitive('hello')).toBe(true);
		expect(isPrimitive(42)).toBe(true);
		expect(isPrimitive(true)).toBe(true);
	});

	it('isPrimitive: objects and arrays are not primitive', () => {
		expect(isPrimitive({})).toBe(false);
		expect(isPrimitive([])).toBe(false);
		expect(isPrimitive({ a: 1 })).toBe(false);
	});

	it('isFileRef: matches {url: string}', () => {
		expect(isFileRef({ url: 'https://example.com/file.pdf', filename: 'file.pdf' })).toBe(true);
		expect(isFileRef({ url: 'https://example.com' })).toBe(true);
	});

	it('isFileRef: rejects non-file-refs', () => {
		expect(isFileRef(null)).toBe(false);
		expect(isFileRef('https://example.com')).toBe(false);
		expect(isFileRef({ href: 'https://example.com' })).toBe(false);
		expect(isFileRef({})).toBe(false);
	});

	it('isStorageKey: matches known s3 key prefixes', () => {
		expect(isStorageKey('instances/abc-123/node/output.json')).toBe(true);
		expect(isStorageKey('templates/tmpl-xyz/config.json')).toBe(true);
		expect(isStorageKey('artifacts/run-1/result.csv')).toBe(true);
	});

	it('isStorageKey: rejects non-storage strings and non-strings', () => {
		expect(isStorageKey('just a random string')).toBe(false);
		expect(isStorageKey('https://example.com/file.pdf')).toBe(false);
		expect(isStorageKey(42)).toBe(false);
		expect(isStorageKey(null)).toBe(false);
	});
});

describe('jsonSchemaToSchemaNode', () => {
	it('type: string → scalar String', () => {
		const node = jsonSchemaToSchemaNode({ type: 'string' });
		expect(node).toMatchObject({ kind: 'scalar', name: 'String' });
	});

	it('type: number → scalar Number', () => {
		const node = jsonSchemaToSchemaNode({ type: 'number' });
		expect(node).toMatchObject({ kind: 'scalar', name: 'Number' });
	});

	it('type: integer → scalar Number (mirrors backend)', () => {
		const node = jsonSchemaToSchemaNode({ type: 'integer' });
		expect(node).toMatchObject({ kind: 'scalar', name: 'Number' });
	});

	it('type: boolean → scalar Bool', () => {
		const node = jsonSchemaToSchemaNode({ type: 'boolean' });
		expect(node).toMatchObject({ kind: 'scalar', name: 'Bool' });
	});

	it('type: object with properties → object SchemaNode', () => {
		const schema = {
			type: 'object',
			properties: {
				name: { type: 'string' },
				count: { type: 'integer' }
			}
		};
		const node = jsonSchemaToSchemaNode(schema);
		expect(node.kind).toBe('object');
		if (node.kind === 'object') {
			expect(node.fields.has('name')).toBe(true);
			expect(node.fields.has('count')).toBe(true);
			expect(node.fields.get('name')).toMatchObject({ kind: 'scalar', name: 'String' });
			expect(node.fields.get('count')).toMatchObject({ kind: 'scalar', name: 'Number' });
		}
	});

	it('properties without type: object → object SchemaNode', () => {
		const schema = { properties: { x: { type: 'string' } } };
		const node = jsonSchemaToSchemaNode(schema);
		expect(node.kind).toBe('object');
	});

	it('type: array with items → array SchemaNode', () => {
		const schema = { type: 'array', items: { type: 'string' } };
		const node = jsonSchemaToSchemaNode(schema);
		expect(node.kind).toBe('array');
		if (node.kind === 'array') {
			expect(node.element).toMatchObject({ kind: 'scalar', name: 'String' });
			expect(node.label).toBe('array<String>');
		}
	});

	it('array without items → array<any>', () => {
		const node = jsonSchemaToSchemaNode({ type: 'array' });
		expect(node.kind).toBe('array');
		if (node.kind === 'array') {
			expect(node.element.kind).toBe('any');
		}
	});

	it('enum of strings → scalar String', () => {
		const node = jsonSchemaToSchemaNode({ enum: ['a', 'b', 'c'] });
		expect(node).toMatchObject({ kind: 'scalar', name: 'String' });
	});

	it('enum of numbers → scalar Number', () => {
		const node = jsonSchemaToSchemaNode({ enum: [1, 2, 3] });
		expect(node).toMatchObject({ kind: 'scalar', name: 'Number' });
	});

	it('oneOf → any', () => {
		const node = jsonSchemaToSchemaNode({ oneOf: [{ type: 'string' }, { type: 'number' }] });
		expect(node.kind).toBe('any');
	});

	it('anyOf → any', () => {
		const node = jsonSchemaToSchemaNode({ anyOf: [{ type: 'string' }] });
		expect(node.kind).toBe('any');
	});

	it('allOf → any', () => {
		const node = jsonSchemaToSchemaNode({ allOf: [{ type: 'string' }] });
		expect(node.kind).toBe('any');
	});

	it('null / undefined / non-object → any', () => {
		expect(jsonSchemaToSchemaNode(null).kind).toBe('any');
		expect(jsonSchemaToSchemaNode(undefined).kind).toBe('any');
		expect(jsonSchemaToSchemaNode('string literal').kind).toBe('any');
		expect(jsonSchemaToSchemaNode(42).kind).toBe('any');
	});

	it('$ref resolves against definitions', () => {
		const defs = { Foo: { type: 'string' } };
		const schema = { $ref: '#/definitions/Foo' };
		const node = jsonSchemaToSchemaNode(schema, defs);
		expect(node).toMatchObject({ kind: 'scalar', name: 'String' });
	});

	it('$ref resolves against $defs', () => {
		const defs = { Bar: { type: 'integer' } };
		const schema = { $ref: '#/$defs/Bar' };
		const node = jsonSchemaToSchemaNode(schema, defs);
		expect(node).toMatchObject({ kind: 'scalar', name: 'Number' });
	});

	it('$ref to unknown definition → any', () => {
		const node = jsonSchemaToSchemaNode({ $ref: '#/definitions/Missing' }, {});
		expect(node.kind).toBe('any');
	});

	it('$ref without definitions → any', () => {
		const node = jsonSchemaToSchemaNode({ $ref: '#/definitions/Foo' });
		expect(node.kind).toBe('any');
	});

	it('nested object via $ref resolves recursively', () => {
		const defs = {
			Address: {
				type: 'object',
				properties: {
					street: { type: 'string' },
					zip: { type: 'string' }
				}
			}
		};
		const schema = {
			type: 'object',
			properties: {
				home: { $ref: '#/definitions/Address' }
			}
		};
		const node = jsonSchemaToSchemaNode(schema, defs);
		expect(node.kind).toBe('object');
		if (node.kind === 'object') {
			const homeNode = node.fields.get('home');
			expect(homeNode?.kind).toBe('object');
			if (homeNode?.kind === 'object') {
				expect(homeNode.fields.has('street')).toBe(true);
			}
		}
	});
});

describe('portToSchemaNode', () => {
	it('builds object node from port fields', () => {
		const port: Port = {
			id: 'out',
			label: 'Output',
			fields: [
				{ name: 'name', label: 'Name', kind: 'text' },
				{ name: 'age', label: 'Age', kind: 'number' }
			]
		};
		const node = portToSchemaNode(port);
		expect(node.kind).toBe('object');
		if (node.kind === 'object') {
			expect(node.selectable).toBe(true);
			expect(node.fields.has('name')).toBe(true);
			expect(node.fields.has('age')).toBe(true);
			expect(node.fields.get('name')).toMatchObject({ kind: 'scalar', name: 'String' });
			expect(node.fields.get('age')).toMatchObject({ kind: 'scalar', name: 'Number' });
		}
	});

	it('text/textarea/select/signature → String', () => {
		const kinds = ['text', 'textarea', 'select', 'signature'] as const;
		for (const kind of kinds) {
			const port: Port = {
				id: 'p',
				label: 'P',
				fields: [{ name: 'f', label: 'F', kind }]
			};
			const node = portToSchemaNode(port);
			if (node.kind === 'object') {
				expect(node.fields.get('f')).toMatchObject({ kind: 'scalar', name: 'String' });
			}
		}
	});

	it('bool → Bool', () => {
		const port: Port = {
			id: 'p',
			label: 'P',
			fields: [{ name: 'active', label: 'Active', kind: 'bool' }]
		};
		const node = portToSchemaNode(port);
		if (node.kind === 'object') {
			expect(node.fields.get('active')).toMatchObject({ kind: 'scalar', name: 'Bool' });
		}
	});

	it('file → FileRef', () => {
		const port: Port = {
			id: 'p',
			label: 'P',
			fields: [{ name: 'attachment', label: 'Attachment', kind: 'file' }]
		};
		const node = portToSchemaNode(port);
		if (node.kind === 'object') {
			expect(node.fields.get('attachment')).toMatchObject({ kind: 'scalar', name: 'FileRef' });
		}
	});

	it('timestamp → Timestamp', () => {
		// 'timestamp' is the only date-like FieldKind (there is no 'date' in the enum).
		const port: Port = {
			id: 'p',
			label: 'P',
			fields: [{ name: 'created_at', label: 'Created', kind: 'timestamp' }]
		};
		const node = portToSchemaNode(port);
		if (node.kind === 'object') {
			expect(node.fields.get('created_at')).toMatchObject({ kind: 'scalar', name: 'Timestamp' });
		}
	});

	it('json → any', () => {
		const port: Port = {
			id: 'p',
			label: 'P',
			fields: [{ name: 'payload', label: 'Payload', kind: 'json' }]
		};
		const node = portToSchemaNode(port);
		if (node.kind === 'object') {
			expect(node.fields.get('payload')).toMatchObject({ kind: 'any' });
		}
	});

	it('field with schema override uses jsonSchemaToSchemaNode', () => {
		const port: Port = {
			id: 'p',
			label: 'P',
			fields: [
				{
					name: 'steps',
					label: 'Steps',
					kind: 'json',
					schema: {
						type: 'array',
						items: { type: 'object', properties: { label: { type: 'string' } } }
					}
				}
			]
		};
		const node = portToSchemaNode(port);
		if (node.kind === 'object') {
			const stepsNode = node.fields.get('steps');
			expect(stepsNode?.kind).toBe('array');
		}
	});

	it('empty port (no fields) → object with zero fields', () => {
		const port: Port = { id: 'p', label: 'P', fields: [] };
		const node = portToSchemaNode(port);
		expect(node.kind).toBe('object');
		if (node.kind === 'object') {
			expect(node.fields.size).toBe(0);
		}
	});

	it('undefined fields → object with zero fields', () => {
		const port: Port = { id: 'p', label: 'P' };
		const node = portToSchemaNode(port);
		expect(node.kind).toBe('object');
		if (node.kind === 'object') {
			expect(node.fields.size).toBe(0);
		}
	});
});

describe('fileMetadataDataTypeToSchemaNode', () => {
	it('maps unit (snake_case string) variants to labelled scalars', () => {
		for (const name of ['string', 'int64', 'float64', 'boolean', 'binary']) {
			const node = fileMetadataDataTypeToSchemaNode(name);
			expect(node.kind).toBe('scalar');
			expect(node.label).toBe(name);
		}
	});

	it('maps a struct variant to a recursive object node', () => {
		const node = fileMetadataDataTypeToSchemaNode({
			struct: [
				['name', 'string'],
				['age', 'int64'],
				['address', { struct: [['city', 'string']] }]
			]
		});
		expect(node.kind).toBe('object');
		if (node.kind === 'object') {
			expect([...node.fields.keys()]).toEqual(['name', 'age', 'address']);
			expect(node.fields.get('age')?.label).toBe('int64');
			const addr = node.fields.get('address');
			expect(addr?.kind).toBe('object');
			if (addr?.kind === 'object') {
				expect(addr.fields.get('city')?.label).toBe('string');
			}
		}
	});

	it('maps a list variant to an array node with element schema', () => {
		const node = fileMetadataDataTypeToSchemaNode({ list: 'string' });
		expect(node.kind).toBe('array');
		if (node.kind === 'array') {
			expect(node.element.label).toBe('string');
			expect(node.label).toBe('list<string>');
		}
	});

	it('folds a timestamp timezone into the label', () => {
		expect(fileMetadataDataTypeToSchemaNode({ timestamp: { timezone: 'UTC' } }).label).toBe(
			'timestamp<UTC>'
		);
		expect(fileMetadataDataTypeToSchemaNode({ timestamp: { timezone: null } }).label).toBe(
			'timestamp'
		);
	});

	it('unwraps a dictionary to its value type and maps unknown to opaque', () => {
		expect(
			fileMetadataDataTypeToSchemaNode({ dictionary: { index: 'uint32', value: 'string' } }).label
		).toBe('string');
		const unk = fileMetadataDataTypeToSchemaNode({ unknown: 'custom' });
		expect(unk.kind).toBe('opaque');
		expect(unk.label).toBe('custom');
	});

	it('degrades unrecognized input to any', () => {
		expect(fileMetadataDataTypeToSchemaNode(null).kind).toBe('any');
		expect(fileMetadataDataTypeToSchemaNode(42).kind).toBe('any');
		expect(fileMetadataDataTypeToSchemaNode({ weird: 1 }).kind).toBe('any');
	});
});

describe('catalogueColumnsToSchemaNode', () => {
	it('builds an object node from file-metadata columns', () => {
		const node = catalogueColumnsToSchemaNode([
			{ name: 'id', data_type: 'int64', nullable: false },
			{ name: 'meta', data_type: { struct: [['k', 'string']] }, nullable: true }
		]);
		expect(node?.kind).toBe('object');
		if (node?.kind === 'object') {
			expect(node.fields.get('id')?.label).toBe('int64');
			expect(node.fields.get('meta')?.kind).toBe('object');
		}
	});

	it('returns null for empty / non-record / malformed columns', () => {
		expect(catalogueColumnsToSchemaNode([])).toBeNull();
		expect(catalogueColumnsToSchemaNode(undefined)).toBeNull();
		expect(catalogueColumnsToSchemaNode('nope')).toBeNull();
		expect(catalogueColumnsToSchemaNode([{ nope: 1 }])).toBeNull();
	});
});
