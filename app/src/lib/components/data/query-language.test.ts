import { describe, it, expect } from 'vitest';
import {
	parseQuery,
	formatQuery,
	compileQuery,
	validateTerms,
	removeTerm,
	addTerm,
	type QueryTerm
} from './query-language';

const NOW = new Date('2026-06-10T12:00:00.000Z');

/** Parse, assert no errors, return terms. */
function terms(input: string): QueryTerm[] {
	const r = parseQuery(input);
	expect(r.errors).toEqual([]);
	return r.terms;
}

/** Parse + compile in one go with the fixed clock. */
function compile(input: string) {
	return compileQuery(terms(input), NOW);
}

describe('parseQuery — search terms', () => {
	it('treats bare words as free-text search terms', () => {
		expect(terms('hello')).toEqual([{ kind: 'search', text: 'hello', raw: 'hello' }]);
	});

	it('treats quoted strings (with spaces) as one search term', () => {
		expect(terms('"hello big world"')).toEqual([
			{ kind: 'search', text: 'hello big world', raw: '"hello big world"' }
		]);
	});

	it('accumulates multiple search terms', () => {
		const t = terms('hello "big world" foo');
		expect(t.map((x) => x.kind)).toEqual(['search', 'search', 'search']);
	});

	it('quoted text containing op characters stays a search term', () => {
		expect(terms('"a:b"')).toEqual([{ kind: 'search', text: 'a:b', raw: '"a:b"' }]);
	});

	it('returns nothing for empty / whitespace-only input', () => {
		expect(parseQuery('')).toEqual({ terms: [], errors: [] });
		expect(parseQuery('   \t \n ')).toEqual({ terms: [], errors: [] });
	});
});

describe('parseQuery — filter ops', () => {
	it.each([
		['name:alice', 'name', 'eq', 'alice'],
		['name!=bob', 'name', 'ne', 'bob'],
		['name!:bob', 'name', 'ne', 'bob'],
		['count>5', 'count', 'gt', '5'],
		['count>=5', 'count', 'gte', '5'],
		['count<5', 'count', 'lt', '5'],
		['count<=5', 'count', 'lte', '5']
	] as const)('%s → %s %s %s', (input, field, op, value) => {
		expect(terms(input)).toEqual([{ kind: 'filter', field, op, value, raw: input }]);
	});

	it('parses unquoted comma lists as in / not_in', () => {
		expect(terms('ext:csv,json')).toEqual([
			{ kind: 'filter', field: 'ext', op: 'in', value: 'csv,json', raw: 'ext:csv,json' }
		]);
		expect(terms('ext!=csv,json')).toEqual([
			{ kind: 'filter', field: 'ext', op: 'not_in', value: 'csv,json', raw: 'ext!=csv,json' }
		]);
		expect(terms('ext!:a,b,c')[0]).toMatchObject({ op: 'not_in', value: 'a,b,c' });
	});

	it('parses field:null / field:* as null checks', () => {
		expect(terms('owner:null')).toEqual([
			{ kind: 'filter', field: 'owner', op: 'is_null', value: '', raw: 'owner:null' }
		]);
		expect(terms('owner:*')).toEqual([
			{ kind: 'filter', field: 'owner', op: 'is_not_null', value: '', raw: 'owner:*' }
		]);
	});

	it('quoting opts out of the special forms (null, *, comma list)', () => {
		expect(terms('owner:"null"')[0]).toMatchObject({ op: 'eq', value: 'null' });
		expect(terms('owner:"*"')[0]).toMatchObject({ op: 'eq', value: '*' });
		expect(terms('ext:"a,b"')[0]).toMatchObject({ op: 'eq', value: 'a,b' });
	});

	it('parses quoted values with spaces', () => {
		expect(terms('name:"Alice Smith"')).toEqual([
			{ kind: 'filter', field: 'name', op: 'eq', value: 'Alice Smith', raw: 'name:"Alice Smith"' }
		]);
	});

	it('parses dotted meta.* fields', () => {
		expect(terms('meta.num_rows>100')[0]).toMatchObject({
			field: 'meta.num_rows',
			op: 'gt',
			value: '100'
		});
		expect(terms('meta.schema.version:2')[0]).toMatchObject({
			field: 'meta.schema.version',
			op: 'eq'
		});
	});

	it('allows negative numbers in comparisons', () => {
		expect(terms('delta>-5')[0]).toMatchObject({ op: 'gt', value: '-5' });
	});
});

describe('parseQuery — containment sugar', () => {
	it('parses all five sugars', () => {
		expect(terms('format:CSV')[0]).toEqual({
			kind: 'contain',
			term: 'format',
			value: 'CSV',
			raw: 'format:CSV'
		});
		expect(terms('col:age')[0]).toEqual({
			kind: 'contain',
			term: 'col',
			value: 'age',
			raw: 'col:age'
		});
		expect(terms('dim:time')[0]).toEqual({
			kind: 'contain',
			term: 'dim',
			value: 'time',
			raw: 'dim:time'
		});
		expect(terms('pii:EMAIL')[0]).toEqual({
			kind: 'contain',
			term: 'pii',
			value: 'EMAIL',
			raw: 'pii:EMAIL'
		});
		expect(terms('attr:source=manual')[0]).toEqual({
			kind: 'contain',
			term: 'attr',
			key: 'source',
			value: 'manual',
			raw: 'attr:source=manual'
		});
	});

	it('supports quoted sugar values with spaces', () => {
		expect(terms('col:"my col"')[0]).toMatchObject({ term: 'col', value: 'my col' });
		expect(terms('attr:note="hello world"')[0]).toMatchObject({
			term: 'attr',
			key: 'note',
			value: 'hello world'
		});
	});

	it('format with a non-: op is a plain filter, not sugar', () => {
		expect(terms('format!=csv')[0]).toMatchObject({ kind: 'filter', field: 'format', op: 'ne' });
	});

	it('flags a second format term as a duplicate (scalar conflict)', () => {
		const r = parseQuery('format:csv format:parquet');
		expect(r.terms).toHaveLength(1);
		expect(r.terms[0]).toMatchObject({ kind: 'contain', term: 'format', value: 'csv' });
		expect(r.errors).toEqual([
			{ raw: 'format:parquet', index: 11, message: 'duplicate format term' }
		]);
	});

	it('flags a re-assigned attr key as a duplicate', () => {
		const r = parseQuery('attr:k=v1 attr:k=v2');
		expect(r.terms).toHaveLength(1);
		expect(r.errors[0]).toMatchObject({ raw: 'attr:k=v2', message: 'duplicate attr term' });
	});

	it('allows repeated array-shaped sugars (cols, dims, pii) and distinct attr keys', () => {
		expect(terms('col:a col:b dim:x dim:y pii:EMAIL pii:SSN attr:a=1 attr:b=2')).toHaveLength(8);
	});
});

describe('parseQuery — comparison value validation', () => {
	it('rejects byte suffixes on non-_bytes fields for comparison ops', () => {
		const r = parseQuery('meta.num_rows>1k');
		expect(r.terms).toEqual([]);
		expect(r.errors).toHaveLength(1);
		expect(r.errors[0].raw).toBe('meta.num_rows>1k');
		expect(r.errors[0].message).toContain('non-numeric');
	});

	it('accepts byte suffixes on *_bytes fields for comparison ops', () => {
		expect(terms('size_bytes>10k')[0]).toMatchObject({ op: 'gt', value: '10k' });
		expect(terms('meta.total_bytes<=1.5g')[0]).toMatchObject({ op: 'lte', value: '1.5g' });
	});

	it('accepts relative and ISO dates on *_at fields for comparison ops', () => {
		expect(terms('created_at>-7d')[0]).toMatchObject({ op: 'gt', value: '-7d' });
		expect(terms('created_at<2026-01-01')[0]).toMatchObject({ op: 'lt', value: '2026-01-01' });
	});

	it('rejects relative dates on non-_at fields and words on numeric comparisons', () => {
		expect(parseQuery('name>-7d').errors).toHaveLength(1);
		expect(parseQuery('name>abc').errors).toHaveLength(1);
	});
});

describe('parseQuery — garbage never throws', () => {
	it.each([
		':::',
		'"unterminated',
		'field>',
		'field:',
		'>foo',
		'attr:noequals',
		'attr:=v',
		'col:',
		'a"b',
		'name:"a"b',
		'<<<>>>',
		'!::!'
	])('%j becomes an error entry, not a throw', (input) => {
		const r = parseQuery(input);
		expect(r.terms).toEqual([]);
		expect(r.errors).toHaveLength(1);
		expect(r.errors[0].raw).toBe(input);
		expect(typeof r.errors[0].message).toBe('string');
	});

	it('keeps good terms alongside error entries, with character indices', () => {
		const r = parseQuery('name:alice ::: count>2');
		expect(r.terms).toHaveLength(2);
		expect(r.errors).toEqual([{ raw: ':::', index: 11, message: 'could not parse term' }]);
	});

	it('handles a token that is just a quote pair', () => {
		const r = parseQuery('""');
		expect(r.errors).toEqual([]);
		expect(r.terms).toEqual([{ kind: 'search', text: '', raw: '""' }]);
	});
});

describe('compileQuery — search and filters', () => {
	it('joins search terms with spaces', () => {
		expect(compile('hello "big world" foo').search).toBe('hello big world foo');
	});

	it('omits search / fileMetadata when absent', () => {
		expect(compileQuery([], NOW)).toEqual({ filters: [] });
		const c = compile('name:alice');
		expect(c.search).toBeUndefined();
		expect(c.fileMetadata).toBeUndefined();
		expect(c.filters).toEqual([{ field: 'name', op: 'eq', value: 'alice' }]);
	});

	it('passes every op through', () => {
		const c = compile('a:1 b!=2 c>3 d>=4 e<5 f<=6 g:x,y h!=x,y i:null j:*');
		expect(c.filters.map((f) => f.op)).toEqual([
			'eq',
			'ne',
			'gt',
			'gte',
			'lt',
			'lte',
			'in',
			'not_in',
			'is_null',
			'is_not_null'
		]);
	});
});

describe('compileQuery — byte-size coercion', () => {
	it.each([
		['size_bytes:10k', '10240'],
		['size_bytes:5m', '5242880'],
		['size_bytes:2g', '2147483648'],
		['size_bytes:1t', '1099511627776'],
		['size_bytes:1.5g', '1610612736'],
		['size_bytes>10K', '10240'],
		['size_bytes>=10kb', '10240'],
		['size_bytes<10KiB', '10240'],
		['meta.total_bytes<=2g', '2147483648']
	])('%s → %s', (input, expected) => {
		expect(compile(input).filters[0].value).toBe(expected);
	});

	it('coerces each element of an in-list', () => {
		expect(compile('size_bytes:1k,2k').filters[0]).toEqual({
			field: 'size_bytes',
			op: 'in',
			value: '1024,2048'
		});
	});

	it('does NOT coerce byte suffixes on non-_bytes fields (eq keeps the literal)', () => {
		expect(compile('meta.num_rows:1k').filters[0].value).toBe('1k');
	});

	it('passes plain numbers through untouched', () => {
		expect(compile('size_bytes>1048576').filters[0].value).toBe('1048576');
	});
});

describe('compileQuery — relative-date coercion (fixed now)', () => {
	it.each([
		['created_at>-7d', '2026-06-03T12:00:00.000Z'],
		['updated_at<-24h', '2026-06-09T12:00:00.000Z'],
		['seen_at>=-90m', '2026-06-10T10:30:00.000Z'],
		['created_at<=-3w', '2026-05-20T12:00:00.000Z'],
		['archived_at>-2y', '2024-06-10T12:00:00.000Z'],
		['created_at:-24h', '2026-06-09T12:00:00.000Z']
	])('%s → %s', (input, expected) => {
		expect(compile(input).filters[0].value).toBe(expected);
	});

	it('does NOT coerce relative-date-shaped values on non-_at fields', () => {
		expect(compile('label:-7d').filters[0].value).toBe('-7d');
	});

	it('is pure in now: same terms, different now, different output', () => {
		const t = terms('created_at>-7d');
		const a = compileQuery(t, new Date('2026-01-08T00:00:00.000Z'));
		expect(a.filters[0].value).toBe('2026-01-01T00:00:00.000Z');
		expect(compileQuery(t, NOW).filters[0].value).toBe('2026-06-03T12:00:00.000Z');
	});
});

describe('compileQuery — fileMetadata containment', () => {
	it('compiles each sugar to its fragment (format lowercased)', () => {
		expect(compile('format:CSV').fileMetadata).toEqual({ format: 'csv' });
		expect(compile('col:age').fileMetadata).toEqual({ column_names: ['age'] });
		expect(compile('dim:time').fileMetadata).toEqual({ dimensions: [{ name: 'time' }] });
		expect(compile('pii:EMAIL').fileMetadata).toEqual({
			columns: [{ classifications: [{ category: 'EMAIL' }] }]
		});
		expect(compile('attr:source=manual').fileMetadata).toEqual({
			attributes: { source: { type: 'String', value: 'manual' } }
		});
	});

	it('deep-merges into ONE object; arrays concatenate', () => {
		expect(compile('col:a col:b').fileMetadata).toEqual({ column_names: ['a', 'b'] });
		expect(compile('pii:EMAIL pii:SSN').fileMetadata).toEqual({
			columns: [
				{ classifications: [{ category: 'EMAIL' }] },
				{ classifications: [{ category: 'SSN' }] }
			]
		});
		expect(compile('format:CSV col:age dim:time attr:a=1 attr:b=2').fileMetadata).toEqual({
			format: 'csv',
			column_names: ['age'],
			dimensions: [{ name: 'time' }],
			attributes: {
				a: { type: 'String', value: '1' },
				b: { type: 'String', value: '2' }
			}
		});
	});

	it('on a scalar conflict fed directly to compile, the first writer wins', () => {
		const t: QueryTerm[] = [
			{ kind: 'contain', term: 'format', value: 'csv', raw: 'format:csv' },
			{ kind: 'contain', term: 'format', value: 'parquet', raw: 'format:parquet' }
		];
		expect(compileQuery(t, NOW).fileMetadata).toEqual({ format: 'csv' });
	});
});

describe('formatQuery — canonical round-trip', () => {
	const battery = [
		'name:alice',
		'name!=bob',
		'size_bytes>10k',
		'size_bytes>=1024',
		'meta.depth<3',
		'meta.depth<=3',
		'ext:csv,json',
		'ext!=csv,json',
		'owner:null',
		'owner:*',
		'name:"Alice Smith"',
		'owner:"null"',
		'"hello world"',
		'"a:b"',
		'hello',
		'format:csv',
		'col:age',
		'col:"my col"',
		'dim:time',
		'pii:EMAIL',
		'attr:source=manual',
		'attr:note="hello world"',
		'created_at>-7d',
		'hello name:alice size_bytes<=1.5g col:a "free text" attr:k=v owner:* ext:a,b'
	];

	it.each(battery)('formatQuery(parseQuery(%j).terms) === input', (input) => {
		expect(formatQuery(terms(input))).toBe(input);
	});

	it.each(battery)('parse(format(parse(%j))) yields identical terms', (input) => {
		const once = terms(input);
		const twice = terms(formatQuery(once));
		expect(twice).toEqual(once);
	});

	it('canonicalizes the != alias for ne', () => {
		expect(formatQuery(terms('name!:bob'))).toBe('name!=bob');
	});

	it('quotes values that would reparse differently', () => {
		const t: QueryTerm[] = [
			{ kind: 'filter', field: 'owner', op: 'eq', value: 'null', raw: 'x' },
			{ kind: 'filter', field: 'ext', op: 'eq', value: 'a,b', raw: 'y' },
			{ kind: 'search', text: 'a:b', raw: 'z' }
		];
		const text = formatQuery(t);
		expect(text).toBe('owner:"null" ext:"a,b" "a:b"');
		expect(parseQuery(text).terms.map((x) => x.kind)).toEqual(['filter', 'filter', 'search']);
	});

	it('formats an empty term list as an empty string', () => {
		expect(formatQuery([])).toBe('');
	});
});

describe('validateTerms', () => {
	it('flags unknown filter fields, with the term index', () => {
		const t = terms('name:alice bogus:1 col:x free meta.rows>5');
		const errs = validateTerms(t, new Set(['name', 'meta.*']));
		expect(errs).toEqual([{ raw: 'bogus:1', index: 1, message: 'unknown field "bogus"' }]);
	});

	it('accepts exact matches and dotted wildcards; skips search/contain terms', () => {
		const t = terms('meta.rows:5 meta.a.b:1 size_bytes>1 format:csv hello');
		expect(validateTerms(t, new Set(['size_bytes', 'meta.*']))).toEqual([]);
		expect(validateTerms(t, new Set(['size_bytes', 'meta.rows']))).toHaveLength(1);
	});

	it('returns every unknown field against an empty registry', () => {
		const t = terms('a:1 b:2');
		expect(validateTerms(t, new Set())).toHaveLength(2);
	});
});

describe('removeTerm', () => {
	it('removes all terms with the matching raw text', () => {
		const t = terms('a:1 b:2 a:1');
		const out = removeTerm(t, 'a:1');
		expect(out).toEqual([{ kind: 'filter', field: 'b', op: 'eq', value: '2', raw: 'b:2' }]);
	});

	it('is a no-op for an unknown raw', () => {
		const t = terms('a:1');
		expect(removeTerm(t, 'z:9')).toEqual(t);
	});
});

describe('addTerm', () => {
	it('appends with a single space', () => {
		expect(addTerm('name:alice', 'format:csv')).toBe('name:alice format:csv');
	});

	it('dedups an identical raw term', () => {
		expect(addTerm('name:alice format:csv', 'format:csv')).toBe('name:alice format:csv');
		expect(addTerm('a:1 "two words" b:2', '"two words"')).toBe('a:1 "two words" b:2');
	});

	it('handles empty / whitespace-only existing text', () => {
		expect(addTerm('', 'x:1')).toBe('x:1');
		expect(addTerm('   ', 'x:1')).toBe('x:1');
	});

	it('ignores an empty new term', () => {
		expect(addTerm('a:1', '   ')).toBe('a:1');
	});
});
