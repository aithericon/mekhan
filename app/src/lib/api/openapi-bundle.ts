/**
 * Parser for the synthesized per-project OpenAPI bundle
 * (`GET /api/v1/workspaces/{ws}/projects/{p}/openapi.json`).
 *
 * The bundle is a free-form OpenAPI 3.0.3 document (not part of `schema.d.ts`),
 * so we normalize it here into the small shape the API drawer + invoke
 * playground render. The server is the single source of truth for the contract;
 * this just flattens it for display and drives the typed invoke form.
 */

export interface ApiField {
	name: string;
	/** JSON Schema `type` (string|number|boolean|object|…). */
	type: string;
	required: boolean;
	format?: string;
	enum?: string[];
	description?: string;
	/** True when the field is a File: a storage-path string in the JSON body,
	 * uploadable as a binary part via `multipart/form-data`. */
	isFile: boolean;
}

export interface ManualEndpoint {
	kind: 'manual';
	nodeId: string;
	title: string;
	templateName?: string;
	firePath?: string;
	invokePath?: string;
	fields: ApiField[];
	hasFile: boolean;
	/** Whether the request body is precisely typed (vs a loose passthrough). */
	typed: boolean;
	security: string[];
	/** JSON Schema of the sync-invoke success envelope's `value`. */
	responseValueSchema?: Record<string, unknown>;
}

export interface WebhookEndpoint {
	kind: 'webhook';
	nodeId: string;
	title: string;
	templateName?: string;
	method: string;
	path: string;
	security: string[];
}

export type Endpoint = ManualEndpoint | WebhookEndpoint;

export interface ParsedBundle {
	title: string;
	description?: string;
	endpoints: Endpoint[];
	securitySchemes: { name: string; type: string; detail: string }[];
}

type Json = Record<string, unknown>;

function asObj(v: unknown): Json | undefined {
	return v && typeof v === 'object' && !Array.isArray(v) ? (v as Json) : undefined;
}

/** Resolve a (possibly `$ref`) schema against `components.schemas`. */
function resolveSchema(schema: unknown, schemas: Json): Json | undefined {
	const s = asObj(schema);
	if (!s) return undefined;
	const ref = typeof s.$ref === 'string' ? s.$ref : undefined;
	if (ref) {
		const name = ref.split('/').pop() ?? '';
		return asObj(schemas[name]);
	}
	return s;
}

/** Security requirement objects → flat list of scheme names. */
function securityNames(op: Json): string[] {
	const sec = op.security;
	if (!Array.isArray(sec)) return [];
	const names = new Set<string>();
	for (const req of sec) {
		const r = asObj(req);
		if (r) for (const k of Object.keys(r)) names.add(k);
	}
	return [...names];
}

function fieldsFromContent(content: Json | undefined, schemas: Json): {
	fields: ApiField[];
	hasFile: boolean;
	typed: boolean;
} {
	const json = asObj(content?.['application/json']);
	const jsonSchema = resolveSchema(json?.schema, schemas);
	const multipart = asObj(content?.['multipart/form-data']);
	const multipartSchema = resolveSchema(multipart?.schema, schemas);

	// File fields are those the multipart variant marks `format: binary`.
	const fileNames = new Set<string>();
	const mpProps = asObj(multipartSchema?.properties);
	if (mpProps) {
		for (const [name, raw] of Object.entries(mpProps)) {
			const p = asObj(raw);
			if (p && p.format === 'binary') fileNames.add(name);
		}
	}

	const props = asObj(jsonSchema?.properties);
	const requiredList = Array.isArray(jsonSchema?.required)
		? (jsonSchema!.required as string[])
		: [];
	const fields: ApiField[] = [];
	if (props) {
		for (const [name, raw] of Object.entries(props)) {
			const p = asObj(raw) ?? {};
			fields.push({
				name,
				type: typeof p.type === 'string' ? p.type : 'object',
				required: requiredList.includes(name),
				format: typeof p.format === 'string' ? p.format : undefined,
				enum: Array.isArray(p.enum) ? (p.enum as unknown[]).map(String) : undefined,
				description: typeof p.description === 'string' ? p.description : undefined,
				isFile: fileNames.has(name)
			});
		}
	}

	// "typed" = a declared object contract (additionalProperties:false). A loose
	// passthrough (additionalProperties:true / no props) is not precise.
	const typed = !!jsonSchema && jsonSchema.additionalProperties === false && fields.length > 0;
	return { fields, hasFile: !!multipart, typed };
}

const HTTP_METHODS = ['get', 'post', 'put', 'patch', 'delete'] as const;

export function parseBundle(doc: Record<string, unknown>): ParsedBundle {
	const info = asObj(doc.info) ?? {};
	const components = asObj(doc.components) ?? {};
	const schemas = asObj(components.schemas) ?? {};
	const paths = asObj(doc.paths) ?? {};

	const securitySchemes: ParsedBundle['securitySchemes'] = [];
	const secObj = asObj(components.securitySchemes);
	if (secObj) {
		for (const [name, raw] of Object.entries(secObj)) {
			const s = asObj(raw) ?? {};
			let detail = String(s.type ?? '');
			if (s.type === 'apiKey') detail = `${s.in} · ${s.name}`;
			else if (s.type === 'http') detail = `${s.scheme}`;
			securitySchemes.push({ name, type: String(s.type ?? ''), detail });
		}
	}

	// Group manual ops by node id (fire + invoke share one trigger); webhooks
	// stand alone.
	const manualByNode = new Map<string, ManualEndpoint>();
	const webhooks: WebhookEndpoint[] = [];

	for (const [path, rawItem] of Object.entries(paths)) {
		const item = asObj(rawItem);
		if (!item) continue;
		for (const method of HTTP_METHODS) {
			const op = asObj(item[method]);
			if (!op) continue;

			const nodeId = String(op['x-mekhan-node-id'] ?? '');
			const tags = Array.isArray(op.tags) ? (op.tags as unknown[]).map(String) : [];
			const templateName = tags.find((t) => t !== 'triggers' && t !== 'webhooks');
			const content = asObj(asObj(op.requestBody)?.content);

			if (path.includes('/triggers/webhook/')) {
				webhooks.push({
					kind: 'webhook',
					nodeId: nodeId || path,
					title: String(op.summary ?? path),
					templateName,
					method: method.toUpperCase(),
					path,
					security: securityNames(op)
				});
				continue;
			}

			const isFire = path.endsWith('/fire');
			const isInvoke = path.endsWith('/invoke');
			if (!isFire && !isInvoke) continue;

			const key = nodeId || path.replace(/\/(fire|invoke)$/, '');
			let ep = manualByNode.get(key);
			if (!ep) {
				const parsed = fieldsFromContent(content, schemas);
				ep = {
					kind: 'manual',
					nodeId: nodeId || key,
					title: String(op.summary ?? key)
						.replace(/\s*\((fire|invoke)\)\s*$/i, '')
						.trim(),
					templateName,
					fields: parsed.fields,
					hasFile: parsed.hasFile,
					typed: parsed.typed,
					security: securityNames(op)
				};
				manualByNode.set(key, ep);
			}
			if (isFire) ep.firePath = path;
			if (isInvoke) {
				ep.invokePath = path;
				// The sync envelope's `value` schema, for the response preview.
				const ok200 = asObj(asObj(asObj(op.responses)?.['200'])?.content);
				const envelope = resolveSchema(asObj(ok200?.['application/json'])?.schema, schemas);
				const valueSchema = asObj(asObj(envelope?.properties)?.value);
				if (valueSchema) ep.responseValueSchema = valueSchema;
			}
		}
	}

	const endpoints: Endpoint[] = [
		...[...manualByNode.values()].sort((a, b) => a.title.localeCompare(b.title)),
		...webhooks.sort((a, b) => a.title.localeCompare(b.title))
	];

	return {
		title: String(info.title ?? 'Project API'),
		description: typeof info.description === 'string' ? info.description : undefined,
		endpoints,
		securitySchemes
	};
}
