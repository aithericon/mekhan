import { describe, it, expect } from 'vitest';
import {
	schemaToBuilderNode,
	builderNodeToSchema,
	uniqueFieldName,
	slugifyFieldName,
	type BuilderObjectNode,
	type BuilderArrayNode,
	type BuilderScalarNode,
	type BuilderRawNode,
	type BuilderUnionNode,
	type BuilderRefNode
} from './builder-model';

// ── schemaToBuilderNode ───────────────────────────────────────────────────────

describe('schemaToBuilderNode', () => {
	it('null / undefined / non-object → empty object node', () => {
		for (const input of [null, undefined, 'string', 42, []]) {
			const node = schemaToBuilderNode(input);
			expect(node.kind).toBe('object');
			if (node.kind === 'object') {
				expect(node.fields).toHaveLength(0);
				expect(node.required.size).toBe(0);
			}
		}
	});

	it('empty object {} → empty editable object node', () => {
		const node = schemaToBuilderNode({});
		expect(node.kind).toBe('object');
	});

	it('type: object with properties → object node with fields', () => {
		const schema = {
			type: 'object',
			properties: {
				name: { type: 'string' },
				age: { type: 'integer' }
			},
			required: ['name']
		};
		const node = schemaToBuilderNode(schema);
		expect(node.kind).toBe('object');
		if (node.kind !== 'object') return;
		expect(node.fields).toHaveLength(2);
		expect(node.fields[0].name).toBe('name');
		expect(node.fields[1].name).toBe('age');
		expect(node.required.has('name')).toBe(true);
		expect(node.required.has('age')).toBe(false);
	});

	it('type: object sealed → sealed flag', () => {
		const schema = { type: 'object', properties: {}, additionalProperties: false };
		const node = schemaToBuilderNode(schema);
		if (node.kind === 'object') expect(node.sealed).toBe(true);
	});

	it('type: array with items → array node', () => {
		const schema = { type: 'array', items: { type: 'string' } };
		const node = schemaToBuilderNode(schema);
		expect(node.kind).toBe('array');
		if (node.kind === 'array') {
			expect(node.items.kind).toBe('scalar');
			if (node.items.kind === 'scalar') expect(node.items.type).toBe('string');
		}
	});

	it('type: array without items → array node with empty object items', () => {
		const node = schemaToBuilderNode({ type: 'array' });
		expect(node.kind).toBe('array');
		if (node.kind === 'array') {
			expect(node.items.kind).toBe('object');
		}
	});

	it('type: array with minItems/maxItems → array node constraints', () => {
		const schema = { type: 'array', items: { type: 'string' }, minItems: 1, maxItems: 10 };
		const node = schemaToBuilderNode(schema);
		if (node.kind === 'array') {
			expect(node.minItems).toBe(1);
			expect(node.maxItems).toBe(10);
		}
	});

	it('type: string → scalar node', () => {
		const node = schemaToBuilderNode({ type: 'string' });
		expect(node.kind).toBe('scalar');
		if (node.kind === 'scalar') expect(node.type).toBe('string');
	});

	it('type: number → scalar node', () => {
		const node = schemaToBuilderNode({ type: 'number' });
		expect(node.kind).toBe('scalar');
		if (node.kind === 'scalar') expect(node.type).toBe('number');
	});

	it('type: integer → scalar node', () => {
		const node = schemaToBuilderNode({ type: 'integer' });
		expect(node.kind).toBe('scalar');
		if (node.kind === 'scalar') expect(node.type).toBe('integer');
	});

	it('type: boolean → scalar node', () => {
		const node = schemaToBuilderNode({ type: 'boolean' });
		expect(node.kind).toBe('scalar');
		if (node.kind === 'scalar') expect(node.type).toBe('boolean');
	});

	it('nullable type array [string, null] → nullable scalar', () => {
		const node = schemaToBuilderNode({ type: ['string', 'null'] });
		expect(node.kind).toBe('scalar');
		if (node.kind === 'scalar') {
			expect(node.nullable).toBe(true);
			expect(node.type).toBe('string');
		}
	});

	it('nullable type array [null, object] → nullable object', () => {
		const node = schemaToBuilderNode({ type: ['object', 'null'], properties: {} });
		expect(node.kind).toBe('object');
		if (node.kind === 'object') {
			expect(node.nullable).toBe(true);
		}
	});

	it('multi-type array (not [T, null]) → raw node', () => {
		const node = schemaToBuilderNode({ type: ['string', 'number'] });
		expect(node.kind).toBe('raw');
	});

	it('$ref → ref node', () => {
		const node = schemaToBuilderNode({ $ref: '#/definitions/Foo' });
		expect(node.kind).toBe('ref');
		if (node.kind === 'ref') {
			expect(node.name).toBe('Foo');
		}
	});

	it('$ref #/$defs/ → ref node (normalised to definitions name)', () => {
		const node = schemaToBuilderNode({ $ref: '#/$defs/Bar' });
		expect(node.kind).toBe('ref');
		if (node.kind === 'ref') {
			expect(node.name).toBe('Bar');
		}
	});

	it('external $ref → raw node', () => {
		const node = schemaToBuilderNode({ $ref: 'https://example.com/schema' });
		expect(node.kind).toBe('raw');
	});

	it('oneOf → union node', () => {
		const node = schemaToBuilderNode({ oneOf: [{ type: 'string' }, { type: 'integer' }] });
		expect(node.kind).toBe('union');
		if (node.kind === 'union') {
			expect(node.combinator).toBe('oneOf');
			expect(node.variants).toHaveLength(2);
			expect(node.variants[0].kind).toBe('scalar');
			expect(node.variants[1].kind).toBe('scalar');
		}
	});

	it('anyOf → union node', () => {
		const node = schemaToBuilderNode({ anyOf: [{ type: 'string' }] });
		expect(node.kind).toBe('union');
		if (node.kind === 'union') {
			expect(node.combinator).toBe('anyOf');
		}
	});

	it('allOf → raw node', () => {
		const node = schemaToBuilderNode({ allOf: [{ type: 'string' }] });
		expect(node.kind).toBe('raw');
	});

	it('union preserves unmodeled sibling keys across a round-trip', () => {
		const schema = {
			title: 'Choice',
			default: 'a',
			'x-vendor': { hint: 1 },
			description: 'pick one',
			oneOf: [{ type: 'string' }, { type: 'integer' }]
		};
		const node = schemaToBuilderNode(schema);
		expect(node.kind).toBe('union');
		if (node.kind === 'union') {
			expect(node.extra).toMatchObject({ title: 'Choice', default: 'a', 'x-vendor': { hint: 1 } });
			expect(node.description).toBe('pick one');
		}
		// Round-trip must not drop the sibling keys.
		expect(builderNodeToSchema(node)).toEqual(schema);
	});

	it('oneOf with discriminator → union node with discriminator detected', () => {
		const node = schemaToBuilderNode({
			oneOf: [
				{
					type: 'object',
					properties: {
						flavor: { enum: ['a'] },
						value: { type: 'string' }
					}
				},
				{
					type: 'object',
					properties: {
						flavor: { enum: ['b'] },
						count: { type: 'integer' }
					}
				}
			]
		});
		expect(node.kind).toBe('union');
		if (node.kind === 'union') {
			expect(node.discriminator).toBe('flavor');
		}
	});

	it('x-field-kind preserved on scalar', () => {
		const node = schemaToBuilderNode({ type: 'string', 'x-field-kind': 'textarea' });
		if (node.kind === 'scalar') {
			expect(node.fieldKindHint).toBe('textarea');
		}
	});

	it('enum array preserved on scalar', () => {
		const node = schemaToBuilderNode({ type: 'string', enum: ['a', 'b', 'c'] });
		if (node.kind === 'scalar') {
			expect(node.enumValues).toEqual(['a', 'b', 'c']);
		}
	});

	it('numeric constraints preserved', () => {
		const node = schemaToBuilderNode({ type: 'number', minimum: 0, maximum: 100 });
		if (node.kind === 'scalar') {
			expect(node.minimum).toBe(0);
			expect(node.maximum).toBe(100);
		}
	});

	it('string constraints preserved', () => {
		const node = schemaToBuilderNode({
			type: 'string',
			minLength: 1,
			maxLength: 255,
			pattern: '^[a-z]+$'
		});
		if (node.kind === 'scalar') {
			expect(node.minLength).toBe(1);
			expect(node.maxLength).toBe(255);
			expect(node.pattern).toBe('^[a-z]+$');
		}
	});

	it('description preserved across all types', () => {
		const desc = 'A helpful description';
		for (const schema of [
			{ type: 'object', description: desc },
			{ type: 'array', items: { type: 'string' }, description: desc },
			{ type: 'string', description: desc }
		]) {
			const node = schemaToBuilderNode(schema);
			if (
				node.kind === 'object' ||
				node.kind === 'array' ||
				node.kind === 'scalar' ||
				node.kind === 'union'
			) {
				expect(node.description).toBe(desc);
			}
		}
	});

	it('nested object recursion', () => {
		const schema = {
			type: 'object',
			properties: {
				address: {
					type: 'object',
					properties: {
						street: { type: 'string' },
						zip: { type: 'string' }
					}
				}
			}
		};
		const node = schemaToBuilderNode(schema);
		expect(node.kind).toBe('object');
		if (node.kind !== 'object') return;
		const addrField = node.fields.find((f) => f.name === 'address');
		expect(addrField?.node.kind).toBe('object');
		if (addrField?.node.kind === 'object') {
			expect(addrField.node.fields).toHaveLength(2);
		}
	});
});

// ── builderNodeToSchema ───────────────────────────────────────────────────────

describe('builderNodeToSchema', () => {
	it('raw node → pass-through', () => {
		const raw = { $ref: '#/definitions/Foo' };
		const node: BuilderRawNode = { kind: 'raw', reason: 'uses $ref', raw };
		expect(builderNodeToSchema(node)).toBe(raw);
	});

	it('empty object node → {type: "object"}', () => {
		const node: BuilderObjectNode = {
			kind: 'object',
			fields: [],
			required: new Set(),
			sealed: false,
			nullable: false
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['type']).toBe('object');
		expect(schema['properties']).toBeUndefined();
		expect(schema['required']).toBeUndefined();
		expect(schema['additionalProperties']).toBeUndefined();
	});

	it('object with fields → {type: "object", properties: {...}}', () => {
		const node: BuilderObjectNode = {
			kind: 'object',
			fields: [
				{ id: 'x', name: 'name', node: { kind: 'scalar', type: 'string', nullable: false, enumValues: [] } },
				{ id: 'y', name: 'age', node: { kind: 'scalar', type: 'integer', nullable: false, enumValues: [] } }
			],
			required: new Set(['name']),
			sealed: true,
			nullable: false
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['type']).toBe('object');
		const props = schema['properties'] as Record<string, unknown>;
		expect(props).toBeDefined();
		expect(Object.keys(props)).toContain('name');
		expect(Object.keys(props)).toContain('age');
		expect(schema['required']).toEqual(['name']);
		expect(schema['additionalProperties']).toBe(false);
	});

	it('nullable object → type: ["object", "null"]', () => {
		const node: BuilderObjectNode = {
			kind: 'object',
			fields: [],
			required: new Set(),
			sealed: false,
			nullable: true
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['type']).toEqual(['object', 'null']);
	});

	it('array node → {type: "array", items: ...}', () => {
		const itemNode: BuilderScalarNode = {
			kind: 'scalar',
			type: 'string',
			nullable: false,
			enumValues: []
		};
		const node: BuilderArrayNode = {
			kind: 'array',
			items: itemNode,
			nullable: false
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['type']).toBe('array');
		const items = schema['items'] as Record<string, unknown>;
		expect(items['type']).toBe('string');
	});

	it('nullable array → type: ["array", "null"]', () => {
		const node: BuilderArrayNode = {
			kind: 'array',
			items: { kind: 'object', fields: [], required: new Set(), sealed: false, nullable: false },
			nullable: true
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['type']).toEqual(['array', 'null']);
	});

	it('scalar node → {type: "string"}', () => {
		const node: BuilderScalarNode = { kind: 'scalar', type: 'string', nullable: false, enumValues: [] };
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['type']).toBe('string');
	});

	it('nullable scalar → type: ["string", "null"]', () => {
		const node: BuilderScalarNode = { kind: 'scalar', type: 'string', nullable: true, enumValues: [] };
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['type']).toEqual(['string', 'null']);
	});

	it('scalar with x-field-kind → emits x-field-kind', () => {
		const node: BuilderScalarNode = {
			kind: 'scalar',
			type: 'string',
			nullable: false,
			enumValues: [],
			fieldKindHint: 'textarea'
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['x-field-kind']).toBe('textarea');
	});

	it('scalar with enum values → emits enum array', () => {
		const node: BuilderScalarNode = {
			kind: 'scalar',
			type: 'string',
			nullable: false,
			enumValues: ['a', 'b', 'c']
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['enum']).toEqual(['a', 'b', 'c']);
	});

	it('numeric enum values are coerced to numbers', () => {
		const node: BuilderScalarNode = {
			kind: 'scalar',
			type: 'number',
			nullable: false,
			enumValues: ['1', '2', '3']
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['enum']).toEqual([1, 2, 3]);
	});

	it('boolean enum values are coerced to booleans', () => {
		const node: BuilderScalarNode = {
			kind: 'scalar',
			type: 'boolean',
			nullable: false,
			enumValues: ['true', 'false']
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['enum']).toEqual([true, false]);
	});

	it('numeric constraints emitted', () => {
		const node: BuilderScalarNode = {
			kind: 'scalar',
			type: 'number',
			nullable: false,
			enumValues: [],
			minimum: 0,
			maximum: 100
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['minimum']).toBe(0);
		expect(schema['maximum']).toBe(100);
	});

	it('string constraints emitted', () => {
		const node: BuilderScalarNode = {
			kind: 'scalar',
			type: 'string',
			nullable: false,
			enumValues: [],
			minLength: 2,
			maxLength: 50,
			pattern: '^[a-z]+$'
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['minLength']).toBe(2);
		expect(schema['maxLength']).toBe(50);
		expect(schema['pattern']).toBe('^[a-z]+$');
	});

	it('required sorted for deterministic output', () => {
		const node: BuilderObjectNode = {
			kind: 'object',
			fields: [
				{ id: 'x', name: 'z', node: { kind: 'scalar', type: 'string', nullable: false, enumValues: [] } },
				{ id: 'y', name: 'a', node: { kind: 'scalar', type: 'string', nullable: false, enumValues: [] } }
			],
			required: new Set(['z', 'a']),
			sealed: false,
			nullable: false
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['required']).toEqual(['a', 'z']);
	});

	it('union node (oneOf) → {oneOf: [...]}', () => {
		const node: BuilderUnionNode = {
			kind: 'union',
			combinator: 'oneOf',
			variants: [
				{ kind: 'scalar', type: 'string', nullable: false, enumValues: [] },
				{ kind: 'scalar', type: 'integer', nullable: false, enumValues: [] }
			]
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['oneOf']).toHaveLength(2);
		const v0 = (schema['oneOf'] as unknown[])[0] as Record<string, unknown>;
		const v1 = (schema['oneOf'] as unknown[])[1] as Record<string, unknown>;
		expect(v0['type']).toBe('string');
		expect(v1['type']).toBe('integer');
	});

	it('union node (anyOf) → {anyOf: [...]}', () => {
		const node: BuilderUnionNode = {
			kind: 'union',
			combinator: 'anyOf',
			variants: [{ kind: 'scalar', type: 'boolean', nullable: false, enumValues: [] }]
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['anyOf']).toBeDefined();
		expect(schema['oneOf']).toBeUndefined();
	});

	it('union node with description → emits description', () => {
		const node: BuilderUnionNode = {
			kind: 'union',
			combinator: 'oneOf',
			variants: [],
			description: 'A union of types'
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['description']).toBe('A union of types');
	});

	it('ref node → {$ref: "#/definitions/<name>"}', () => {
		const node: BuilderRefNode = { kind: 'ref', name: 'MyType' };
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		expect(schema['$ref']).toBe('#/definitions/MyType');
	});

	it('fields with empty names are excluded from properties', () => {
		const node: BuilderObjectNode = {
			kind: 'object',
			fields: [
				{ id: 'x', name: '', node: { kind: 'scalar', type: 'string', nullable: false, enumValues: [] } },
				{ id: 'y', name: 'valid', node: { kind: 'scalar', type: 'string', nullable: false, enumValues: [] } }
			],
			required: new Set(),
			sealed: false,
			nullable: false
		};
		const schema = builderNodeToSchema(node) as Record<string, unknown>;
		const props = schema['properties'] as Record<string, unknown>;
		expect(Object.keys(props)).toEqual(['valid']);
	});
});

// ── Round-trip fidelity ───────────────────────────────────────────────────────

describe('round-trip fidelity', () => {
	function roundTrip(schema: unknown): unknown {
		return builderNodeToSchema(schemaToBuilderNode(schema));
	}

	it('simple scalar string', () => {
		const schema = { type: 'string', description: 'A name' };
		const out = roundTrip(schema) as Record<string, unknown>;
		expect(out['type']).toBe('string');
		expect(out['description']).toBe('A name');
	});

	it('object with nested object', () => {
		const schema = {
			type: 'object',
			properties: {
				user: {
					type: 'object',
					properties: {
						name: { type: 'string' },
						age: { type: 'integer' }
					},
					required: ['name'],
					additionalProperties: false
				}
			},
			required: ['user']
		};
		const out = roundTrip(schema) as Record<string, unknown>;
		expect(out['type']).toBe('object');
		const props = out['properties'] as Record<string, unknown>;
		expect(props).toBeDefined();
		const user = props['user'] as Record<string, unknown>;
		expect(user['type']).toBe('object');
		expect(user['required']).toEqual(['name']);
		expect(user['additionalProperties']).toBe(false);
		const userProps = user['properties'] as Record<string, unknown>;
		expect(userProps).toBeDefined();
		expect(Object.keys(userProps)).toContain('name');
		expect(Object.keys(userProps)).toContain('age');
	});

	it('array of objects', () => {
		const schema = {
			type: 'array',
			items: {
				type: 'object',
				properties: {
					label: { type: 'string' },
					value: { type: 'number' }
				}
			}
		};
		const out = roundTrip(schema) as Record<string, unknown>;
		expect(out['type']).toBe('array');
		const items = out['items'] as Record<string, unknown>;
		expect(items['type']).toBe('object');
	});

	it('x-field-kind survives round-trip', () => {
		const schema = { type: 'string', 'x-field-kind': 'textarea' };
		const out = roundTrip(schema) as Record<string, unknown>;
		expect(out['x-field-kind']).toBe('textarea');
	});

	it('nullable scalar survives round-trip', () => {
		const schema = { type: ['string', 'null'], description: 'nullable' };
		const out = roundTrip(schema) as Record<string, unknown>;
		expect(out['type']).toEqual(['string', 'null']);
		expect(out['description']).toBe('nullable');
	});

	it('enum survives round-trip', () => {
		const schema = { type: 'string', enum: ['a', 'b', 'c'] };
		const out = roundTrip(schema) as Record<string, unknown>;
		expect(out['enum']).toEqual(['a', 'b', 'c']);
	});

	it('constraints survive round-trip', () => {
		const schema = {
			type: 'number',
			minimum: 0,
			maximum: 100,
			description: 'A percentage'
		};
		const out = roundTrip(schema) as Record<string, unknown>;
		expect(out['minimum']).toBe(0);
		expect(out['maximum']).toBe(100);
		expect(out['description']).toBe('A percentage');
	});

	it('$ref round-trips losslessly (always emits #/definitions/ form)', () => {
		const schema = { $ref: '#/definitions/SomeType' };
		const out = roundTrip(schema) as Record<string, unknown>;
		expect(out['$ref']).toBe('#/definitions/SomeType');
	});

	it('$ref #/$defs/ normalised to #/definitions/ on round-trip', () => {
		const schema = { $ref: '#/$defs/SomeType' };
		const out = roundTrip(schema) as Record<string, unknown>;
		// The builder normalises to #/definitions/ form.
		expect(out['$ref']).toBe('#/definitions/SomeType');
	});

	it('oneOf round-trip: variants + combinator preserved', () => {
		const schema = {
			oneOf: [{ type: 'string' }, { type: 'integer' }],
			description: 'Either a string or int'
		};
		const out = roundTrip(schema) as Record<string, unknown>;
		expect(out['oneOf']).toBeDefined();
		expect((out['oneOf'] as unknown[]).length).toBe(2);
		expect(out['description']).toBe('Either a string or int');
	});

	it('anyOf round-trip: combinator preserved as anyOf', () => {
		const schema = { anyOf: [{ type: 'boolean' }] };
		const out = roundTrip(schema) as Record<string, unknown>;
		expect(out['anyOf']).toBeDefined();
		expect(out['oneOf']).toBeUndefined();
	});

	it('discriminated oneOf round-trip preserves discriminator property', () => {
		const schema = {
			oneOf: [
				{
					type: 'object',
					properties: {
						flavor: { enum: ['a'] },
						value: { type: 'string' }
					},
					required: ['flavor']
				},
				{
					type: 'object',
					properties: {
						flavor: { enum: ['b'] },
						count: { type: 'integer' }
					},
					required: ['flavor']
				}
			]
		};
		const out = roundTrip(schema) as Record<string, unknown>;
		const variants = out['oneOf'] as Record<string, unknown>[];
		expect(variants).toHaveLength(2);
		const props0 = variants[0]['properties'] as Record<string, Record<string, unknown>>;
		expect(props0['flavor']['enum']).toEqual(['a']);
	});

	it('union with nested objects round-trips correctly', () => {
		const schema = {
			oneOf: [
				{ type: 'object', properties: { name: { type: 'string' } } },
				{ type: 'array', items: { type: 'string' } }
			]
		};
		const out = roundTrip(schema) as Record<string, unknown>;
		expect(out['oneOf']).toBeDefined();
		const variants = out['oneOf'] as Record<string, unknown>[];
		expect(variants[0]['type']).toBe('object');
		expect(variants[1]['type']).toBe('array');
	});
});

// ── Utilities ─────────────────────────────────────────────────────────────────

describe('uniqueFieldName', () => {
	it('generates field_1 for empty list', () => {
		expect(uniqueFieldName([])).toBe('field_1');
	});

	it('skips existing names', () => {
		expect(uniqueFieldName(['field_1', 'field_2'])).toBe('field_3');
	});

	it('skips colliding names non-sequentially', () => {
		const name = uniqueFieldName(['field_1', 'field_3']);
		expect(name).toBeTruthy();
		expect(['field_1', 'field_3']).not.toContain(name);
	});
});

describe('slugifyFieldName', () => {
	it('lowercases and replaces spaces', () => {
		expect(slugifyFieldName('Hello World')).toBe('hello_world');
	});

	it('strips leading/trailing underscores', () => {
		expect(slugifyFieldName('_foo_')).toBe('foo');
	});

	it('replaces special chars with underscores', () => {
		expect(slugifyFieldName('my-field.name')).toBe('my_field_name');
	});

	it('passes through clean snake_case', () => {
		expect(slugifyFieldName('my_field')).toBe('my_field');
	});
});
