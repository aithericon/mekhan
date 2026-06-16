/**
 * Catalogue query language — a tiny text DSL that compiles to the catalogue's
 * existing HTTP query params (free-text search, field filters, and a single
 * deep-merged `file_metadata` JSONB containment object with Postgres `@>`
 * AND-semantics).
 *
 * Pure TypeScript, no backend dependency. The parser is FIELD-AGNOSTIC —
 * semantic validation against the server field registry happens in the UI
 * layer via `validateTerms`.
 *
 * Grammar (whitespace-separated terms; double-quoted values may contain
 * spaces; a term that fails to parse becomes an error entry, never a throw):
 *
 *  1. bare word / "quoted string"        → free-text search term
 *  2. field OP value                     → filter term
 *       ops: `:` (eq), `!=` / `!:` (ne), `>`, `>=`, `<`, `<=`
 *       `field:a,b,c` (unquoted list)    → in;   `field!:a,b,c` → not_in
 *       `field:null` → is_null;          `field:*` → is_not_null
 *  3. containment sugar (one merged file_metadata object):
 *       format:VALUE | col:NAME | dim:NAME | pii:CLASS | attr:KEY=VALUE
 *  4. datatype sugar (at most one per query):
 *       datatype:NAME → registered data type; compiled to a meta.schema
 *       filter via a caller-supplied DatatypeResolver (fail-closed when the
 *       name does not resolve)
 *  5. compile-time value coercions:
 *       byte suffixes (10k/5m/2g/1t[b|ib], decimals ok) for *_bytes fields
 *       relative dates (-7d/-24h/-90m/-3w/-2y) for *_at fields
 */

export type QueryOp =
	| 'eq'
	| 'ne'
	| 'gt'
	| 'gte'
	| 'lt'
	| 'lte'
	| 'in'
	| 'not_in'
	| 'is_null'
	| 'is_not_null';

export type QueryTerm =
	| { kind: 'search'; text: string; raw: string }
	| { kind: 'filter'; field: string; op: QueryOp; value: string; raw: string }
	| {
			kind: 'contain';
			term: 'format' | 'col' | 'dim' | 'pii' | 'attr';
			key?: string;
			value: string;
			raw: string;
	  }
	| { kind: 'datatype'; name: string; raw: string };

/**
 * Maps a registered data-type name to its schema-fingerprint digests.
 * `undefined` = unknown name (or registry not loaded) — compile fails closed.
 */
export type DatatypeResolver = (name: string) => string[] | undefined;

export interface ParseError {
	raw: string;
	index: number;
	message: string;
}

export interface CompiledQuery {
	search?: string;
	filters: Array<{ field: string; op: QueryOp; value: string }>;
	fileMetadata?: Record<string, unknown>;
}

type ContainName = 'format' | 'col' | 'dim' | 'pii' | 'attr';

const CONTAIN_TERMS: readonly ContainName[] = ['format', 'col', 'dim', 'pii', 'attr'];

const FILTER_RE = /^([A-Za-z_][A-Za-z0-9_.]*)(>=|<=|!=|!:|>|<|:)(.*)$/;
const NUM_RE = /^-?\d+(\.\d+)?$/;
const BYTE_RE = /^(\d+(\.\d+)?)([kmgt])(i?b)?$/i;
const REL_DATE_RE = /^-(\d+(\.\d+)?)([mhdwy])$/;
const ISO_DATE_RE = /^\d{4}-\d{2}-\d{2}([T ].*)?$/;

const BYTE_EXP: Record<string, number> = { k: 1, m: 2, g: 3, t: 4 };
const REL_DATE_MS: Record<string, number> = {
	m: 60_000,
	h: 3_600_000,
	d: 86_400_000,
	w: 7 * 86_400_000,
	y: 365 * 86_400_000
};

function isBytesField(field: string): boolean {
	return field === 'size_bytes' || field.endsWith('_bytes');
}

function isAtField(field: string): boolean {
	return field.endsWith('_at');
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

interface Token {
	raw: string;
	index: number;
}

/** Split input into whitespace-separated tokens; double quotes glue spaces. */
function tokenize(input: string): Token[] {
	const tokens: Token[] = [];
	let i = 0;
	const n = input.length;
	while (i < n) {
		while (i < n && /\s/.test(input[i])) i++;
		if (i >= n) break;
		const start = i;
		let inQuote = false;
		while (i < n && (inQuote || !/\s/.test(input[i]))) {
			if (input[i] === '"') inQuote = !inQuote;
			i++;
		}
		tokens.push({ raw: input.slice(start, i), index: start });
	}
	return tokens;
}

interface Unquoted {
	value: string;
	quoted: boolean;
	error?: string;
}

/** Strip a single pair of surrounding double quotes, if present. */
function unquote(v: string): Unquoted {
	if (!v.startsWith('"')) return { value: v, quoted: false };
	const end = v.indexOf('"', 1);
	if (end === -1) return { value: v.slice(1), quoted: true, error: 'unterminated quote' };
	if (end !== v.length - 1)
		return { value: v, quoted: true, error: 'unexpected characters after closing quote' };
	return { value: v.slice(1, end), quoted: true };
}

// ---------------------------------------------------------------------------
// Term classification
// ---------------------------------------------------------------------------

type Classified = { term: QueryTerm } | { error: string };

function classifyToken(raw: string): Classified {
	if (raw.startsWith('"')) {
		const u = unquote(raw);
		if (u.error) return { error: u.error };
		return { term: { kind: 'search', text: u.value, raw } };
	}
	const m = FILTER_RE.exec(raw);
	if (m) {
		const [, field, opText, rest] = m;
		// `datatype:` is sugar only with the `:` op; other ops fall through to a
		// plain filter (validateTerms flags the unknown field).
		if (opText === ':' && field === 'datatype') {
			const u = unquote(rest);
			if (u.error) return { error: u.error };
			if (u.value === '') return { error: 'missing value' };
			return { term: { kind: 'datatype', name: u.value, raw } };
		}
		if (opText === ':' && (CONTAIN_TERMS as readonly string[]).includes(field)) {
			return classifyContain(field as ContainName, rest, raw);
		}
		return classifyFilter(field, opText, rest, raw);
	}
	if (/[:<>"]/.test(raw)) return { error: 'could not parse term' };
	return { term: { kind: 'search', text: raw, raw } };
}

function classifyContain(name: ContainName, rest: string, raw: string): Classified {
	if (name === 'attr') {
		const eq = rest.indexOf('=');
		if (eq <= 0) return { error: 'attr term requires KEY=VALUE' };
		const key = rest.slice(0, eq);
		const u = unquote(rest.slice(eq + 1));
		if (u.error) return { error: u.error };
		if (u.value === '') return { error: 'missing value' };
		return { term: { kind: 'contain', term: 'attr', key, value: u.value, raw } };
	}
	const u = unquote(rest);
	if (u.error) return { error: u.error };
	if (u.value === '') return { error: 'missing value' };
	return { term: { kind: 'contain', term: name, value: u.value, raw } };
}

function classifyFilter(field: string, opText: string, rest: string, raw: string): Classified {
	if (rest === '') return { error: 'missing value' };
	const u = unquote(rest);
	if (u.error) return { error: u.error };
	const baseOp: QueryOp =
		opText === ':'
			? 'eq'
			: opText === '!=' || opText === '!:'
				? 'ne'
				: opText === '>'
					? 'gt'
					: opText === '>='
						? 'gte'
						: opText === '<'
							? 'lt'
							: 'lte';
	let op: QueryOp = baseOp;
	const value = u.value;
	if (!u.quoted) {
		// Unquoted-only special forms: quoting opts out of them.
		if (baseOp === 'eq' && value === 'null')
			return { term: { kind: 'filter', field, op: 'is_null', value: '', raw } };
		if (baseOp === 'eq' && value === '*')
			return { term: { kind: 'filter', field, op: 'is_not_null', value: '', raw } };
		if ((baseOp === 'eq' || baseOp === 'ne') && value.includes(',')) {
			op = baseOp === 'eq' ? 'in' : 'not_in';
		}
	}
	if (op === 'gt' || op === 'gte' || op === 'lt' || op === 'lte') {
		if (!isComparableValue(field, value)) {
			return { error: `non-numeric value "${value}" for comparison on field "${field}"` };
		}
	}
	return { term: { kind: 'filter', field, op, value, raw } };
}

/**
 * Comparison ops require a value that compiles to something ordered: a plain
 * number, a byte-suffixed size (only on *_bytes fields), or a relative /
 * ISO date (only on *_at fields).
 */
function isComparableValue(field: string, value: string): boolean {
	if (NUM_RE.test(value)) return true;
	if (isBytesField(field) && BYTE_RE.test(value)) return true;
	if (isAtField(field) && (REL_DATE_RE.test(value) || ISO_DATE_RE.test(value))) return true;
	return false;
}

// ---------------------------------------------------------------------------
// Containment fragments + deep merge
// ---------------------------------------------------------------------------

function containFragment(
	term: Extract<QueryTerm, { kind: 'contain' }>
): Record<string, unknown> {
	switch (term.term) {
		case 'format':
			// Only reached by the parse-time accumulator (duplicate detection);
			// compile routes `format:` to a `meta.format` filter, not containment.
			return { format: term.value };
		case 'col':
			return { column_names: [term.value] };
		case 'dim':
			return { dimensions: [{ name: term.value }] };
		case 'pii':
			return { columns: [{ classifications: [{ category: term.value }] }] };
		case 'attr':
			return { attributes: { [term.key ?? '']: { type: 'String', value: term.value } } };
	}
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
	return typeof v === 'object' && v !== null && !Array.isArray(v);
}

/**
 * Pure deep merge: plain objects merge recursively, arrays CONCATENATE,
 * anything else (scalar vs scalar, scalar vs container) is a conflict.
 */
function tryMerge(a: unknown, b: unknown): { ok: true; value: unknown } | { ok: false } {
	if (a === undefined) return { ok: true, value: b };
	if (Array.isArray(a) && Array.isArray(b)) return { ok: true, value: [...a, ...b] };
	if (isPlainObject(a) && isPlainObject(b)) {
		const out: Record<string, unknown> = { ...a };
		for (const [k, v] of Object.entries(b)) {
			const m = tryMerge(out[k], v);
			if (!m.ok) return { ok: false };
			out[k] = m.value;
		}
		return { ok: true, value: out };
	}
	return { ok: false };
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

export function parseQuery(input: string): { terms: QueryTerm[]; errors: ParseError[] } {
	const terms: QueryTerm[] = [];
	const errors: ParseError[] = [];
	// Running merge of containment fragments so a scalar conflict (e.g. a
	// second `format:` term) is flagged on the LATER term at parse time.
	let containAcc: unknown;
	// Two datatype terms AND-ed match nothing (and the compiled meta.schema
	// params would clobber each other), so the LATER one is flagged too.
	let hasDatatype = false;
	for (const tok of tokenize(input)) {
		const c = classifyToken(tok.raw);
		if ('error' in c) {
			errors.push({ raw: tok.raw, index: tok.index, message: c.error });
			continue;
		}
		const term = c.term;
		if (term.kind === 'contain') {
			const merged = tryMerge(containAcc ?? {}, containFragment(term));
			if (!merged.ok) {
				errors.push({ raw: tok.raw, index: tok.index, message: `duplicate ${term.term} term` });
				continue;
			}
			containAcc = merged.value;
		}
		if (term.kind === 'datatype') {
			if (hasDatatype) {
				errors.push({ raw: tok.raw, index: tok.index, message: 'duplicate datatype term' });
				continue;
			}
			hasDatatype = true;
		}
		terms.push(term);
	}
	return { terms, errors };
}

/** Canonical text for terms; `parseQuery(formatQuery(terms))` round-trips. */
export function formatQuery(terms: QueryTerm[]): string {
	return terms.map(formatTerm).join(' ');
}

function quoteText(v: string): string {
	return `"${v}"`;
}

/** Quote a sugar value so it survives re-tokenization (spaces / quotes / empty). */
export function quoteIfNeeded(v: string): string {
	return v === '' || /[\s"]/.test(v) ? quoteText(v) : v;
}

function formatFilterValue(v: string, op: QueryOp): string {
	const needsQuote =
		v === '' ||
		/[\s"]/.test(v) ||
		((op === 'eq' || op === 'ne') && (v === 'null' || v === '*' || v.includes(',')));
	return needsQuote ? quoteText(v) : v;
}

function formatTerm(t: QueryTerm): string {
	if (t.kind === 'search') {
		if (/^[^\s"]+$/.test(t.text)) {
			const c = classifyToken(t.text);
			if ('term' in c && c.term.kind === 'search') return t.text;
		}
		return quoteText(t.text);
	}
	if (t.kind === 'datatype') {
		return `datatype:${quoteIfNeeded(t.name)}`;
	}
	if (t.kind === 'contain') {
		const v = quoteIfNeeded(t.value);
		return t.term === 'attr' ? `attr:${t.key}=${v}` : `${t.term}:${v}`;
	}
	switch (t.op) {
		case 'is_null':
			return `${t.field}:null`;
		case 'is_not_null':
			return `${t.field}:*`;
		case 'in':
			return `${t.field}:${t.value}`;
		case 'not_in':
			return `${t.field}!=${t.value}`;
		case 'eq':
			return `${t.field}:${formatFilterValue(t.value, 'eq')}`;
		case 'ne':
			return `${t.field}!=${formatFilterValue(t.value, 'ne')}`;
		case 'gt':
			return `${t.field}>${formatFilterValue(t.value, 'gt')}`;
		case 'gte':
			return `${t.field}>=${formatFilterValue(t.value, 'gte')}`;
		case 'lt':
			return `${t.field}<${formatFilterValue(t.value, 'lt')}`;
		case 'lte':
			return `${t.field}<=${formatFilterValue(t.value, 'lte')}`;
	}
}

function coerceValue(field: string, value: string, now: Date): string {
	if (isBytesField(field)) {
		const m = BYTE_RE.exec(value);
		if (m) {
			const exp = BYTE_EXP[m[3].toLowerCase()];
			return String(Math.round(parseFloat(m[1]) * 1024 ** exp));
		}
	}
	if (isAtField(field)) {
		const m = REL_DATE_RE.exec(value);
		if (m) {
			const ms = REL_DATE_MS[m[3]];
			return new Date(now.getTime() - parseFloat(m[1]) * ms).toISOString();
		}
	}
	return value;
}

/**
 * Compile terms into the catalogue's HTTP query params. Pure: relative dates
 * are computed from the `now` parameter (production callers pass the current
 * date; tests pass a fixed one), and `datatype:` terms resolve through the
 * caller-supplied `resolveDatatype`.
 */
export function compileQuery(
	terms: QueryTerm[],
	now: Date = new Date(),
	resolveDatatype?: DatatypeResolver
): CompiledQuery {
	const searchParts: string[] = [];
	const filters: CompiledQuery['filters'] = [];
	let meta: unknown;
	for (const t of terms) {
		if (t.kind === 'search') {
			searchParts.push(t.text);
		} else if (t.kind === 'datatype') {
			const digests = resolveDatatype?.(t.name);
			if (digests && digests.length > 0) {
				filters.push({
					field: 'meta.schema',
					op: digests.length === 1 ? 'eq' : 'in',
					value: digests.join(',')
				});
			} else {
				// FAIL CLOSED: an unresolved data type must match nothing, never
				// silently widen to everything (validateTerms carries the warning).
				filters.push({ field: 'meta.schema', op: 'eq', value: '' });
			}
		} else if (t.kind === 'filter') {
			let value = t.value;
			if (t.op === 'in' || t.op === 'not_in') {
				value = t.value
					.split(',')
					.map((v) => coerceValue(t.field, v, now))
					.join(',');
			} else if (t.op !== 'is_null' && t.op !== 'is_not_null') {
				value = coerceValue(t.field, t.value, now);
			}
			filters.push({ field: t.field, op: t.op, value });
		} else if (t.term === 'format') {
			// `format` is a top-level scalar, not an array/nested containment like
			// col/dim/pii — compile it to a `meta.format` equality instead of a
			// JSONB `@>` fragment. The server unwraps `FileFormat::Unknown`'s
			// `{"unknown":…}` envelope behind that expr, so `format:fasta` matches
			// probe-unknown formats too — which containment never could.
			// Lowercased to match the snake_case `FileFormat` wire strings.
			filters.push({ field: 'meta.format', op: 'eq', value: t.value.toLowerCase() });
		} else {
			// Conflicting fragments were already flagged at parse time; here we
			// keep the first writer and silently skip the conflicting term.
			const merged = tryMerge(meta ?? {}, containFragment(t));
			if (merged.ok) meta = merged.value;
		}
	}
	const out: CompiledQuery = { filters };
	const search = searchParts.join(' ').trim();
	if (search) out.search = search;
	if (meta !== undefined) out.fileMetadata = meta as Record<string, unknown>;
	return out;
}

/**
 * Formats asserted by the terms, for scoping format-specific UI: `format:`
 * sugar (lowercased, matching compile) plus `meta.format` eq / in filters —
 * both compile to the same meta.format equality. Negative / null ops assert
 * nothing. Deduped, order-preserving.
 */
export function activeFormats(terms: QueryTerm[]): string[] {
	const out: string[] = [];
	const push = (v: string) => {
		if (v !== '' && !out.includes(v)) out.push(v);
	};
	for (const t of terms) {
		if (t.kind === 'contain' && t.term === 'format') {
			push(t.value.toLowerCase());
		} else if (t.kind === 'filter' && t.field === 'meta.format') {
			if (t.op === 'eq') push(t.value);
			else if (t.op === 'in') t.value.split(',').forEach(push);
		}
	}
	return out;
}

/**
 * Semantic validation against the server field registry (exact names only)
 * and, when `datatypeNames` is provided, the registered data-type names
 * (`null`/`undefined` registry = skip datatype validation, e.g. not loaded
 * yet). `index` is the term's position in the `terms` array.
 */
export function validateTerms(
	terms: QueryTerm[],
	knownFields: Set<string>,
	datatypeNames?: Set<string> | null
): ParseError[] {
	const errors: ParseError[] = [];
	terms.forEach((t, i) => {
		if (t.kind === 'filter' && !knownFields.has(t.field)) {
			errors.push({ raw: t.raw, index: i, message: `unknown field "${t.field}"` });
		} else if (t.kind === 'datatype' && datatypeNames != null && !datatypeNames.has(t.name)) {
			errors.push({ raw: t.raw, index: i, message: `unknown data type "${t.name}"` });
		}
	});
	return errors;
}

/** Remove every term whose raw text matches `raw`. */
export function removeTerm(terms: QueryTerm[], raw: string): QueryTerm[] {
	return terms.filter((t) => t.raw !== raw);
}

/** Append a term to existing query text, deduping identical raw terms. */
export function addTerm(text: string, term: string): string {
	const t = term.trim();
	if (!t) return text;
	if (tokenize(text).some((tok) => tok.raw === t)) return text;
	const base = text.trim();
	return base ? `${base} ${t}` : t;
}
