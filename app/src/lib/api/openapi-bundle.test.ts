import { describe, it, expect } from 'vitest';
import { parseBundle, type ManualEndpoint, type WebhookEndpoint } from './openapi-bundle';

// A representative slice of the synthesized per-project bundle: one manual
// trigger (typed body + a File field → both content types) and one webhook.
const DOC = {
	openapi: '3.0.3',
	info: { title: 'Project: Demos', description: 'Callable surface.' },
	paths: {
		'/api/v1/triggers/trg_invoice/fire': {
			post: {
				tags: ['triggers', 'Invoices'],
				summary: 'API Call (fire)',
				security: [{ sessionCookie: [] }, { bearerAuth: [] }],
				'x-mekhan-node-id': 'trg_invoice',
				requestBody: {
					content: {
						'application/json': { schema: { $ref: '#/components/schemas/Trigger_trg_invoice_Request' } },
						'multipart/form-data': {
							schema: {
								type: 'object',
								properties: {
									invoice_file: { type: 'string', format: 'binary' },
									invoice_id: { type: 'string' }
								}
							}
						}
					}
				}
			}
		},
		'/api/v1/triggers/trg_invoice/invoke': {
			post: {
				tags: ['triggers', 'Invoices'],
				summary: 'API Call (invoke)',
				security: [{ sessionCookie: [] }, { bearerAuth: [] }],
				'x-mekhan-node-id': 'trg_invoice',
				requestBody: {
					content: {
						'application/json': { schema: { $ref: '#/components/schemas/Trigger_trg_invoice_Request' } }
					}
				},
				responses: {
					'200': {
						content: {
							'application/json': { schema: { $ref: '#/components/schemas/Trigger_trg_invoice_Response' } }
						}
					}
				}
			}
		},
		'/api/triggers/webhook/inbound': {
			post: {
				tags: ['webhooks', 'Hooks'],
				summary: 'Inbound hook',
				security: [],
				'x-mekhan-node-id': 'wh_1',
				requestBody: { content: { 'application/json': { schema: { type: 'object', additionalProperties: true } } } }
			}
		}
	},
	components: {
		schemas: {
			Trigger_trg_invoice_Request: {
				type: 'object',
				additionalProperties: false,
				required: ['invoice_file', 'invoice_id'],
				properties: {
					invoice_file: { type: 'string' },
					invoice_id: { type: 'string' }
				}
			},
			Trigger_trg_invoice_Response: {
				type: 'object',
				properties: { ok: { type: 'boolean', enum: [true] }, value: { type: 'object', additionalProperties: true } }
			}
		},
		securitySchemes: {
			sessionCookie: { type: 'apiKey', in: 'cookie', name: 'mekhan_session' },
			bearerAuth: { type: 'http', scheme: 'bearer' }
		}
	}
};

describe('parseBundle', () => {
	it('groups manual fire+invoke under one trigger with a typed body', () => {
		const parsed = parseBundle(DOC);
		const manual = parsed.endpoints.find((e) => e.kind === 'manual') as ManualEndpoint;
		expect(manual).toBeTruthy();
		expect(manual.nodeId).toBe('trg_invoice');
		expect(manual.title).toBe('API Call');
		expect(manual.firePath).toBe('/api/v1/triggers/trg_invoice/fire');
		expect(manual.invokePath).toBe('/api/v1/triggers/trg_invoice/invoke');
		expect(manual.typed).toBe(true);
		expect(manual.security).toEqual(['sessionCookie', 'bearerAuth']);
		// Fields are typed; the File field is detected via the multipart binary part.
		const byName = Object.fromEntries(manual.fields.map((f) => [f.name, f]));
		expect(byName.invoice_id.type).toBe('string');
		expect(byName.invoice_id.required).toBe(true);
		expect(byName.invoice_file.isFile).toBe(true);
		expect(manual.hasFile).toBe(true);
		expect(manual.responseValueSchema).toBeTruthy();
	});

	it('keeps webhooks separate, async-only, with their derived security', () => {
		const parsed = parseBundle(DOC);
		const hook = parsed.endpoints.find((e) => e.kind === 'webhook') as WebhookEndpoint;
		expect(hook).toBeTruthy();
		expect(hook.method).toBe('POST');
		expect(hook.path).toBe('/api/triggers/webhook/inbound');
		expect(hook.security).toEqual([]);
	});

	it('surfaces security schemes and info', () => {
		const parsed = parseBundle(DOC);
		expect(parsed.title).toBe('Project: Demos');
		expect(parsed.securitySchemes.map((s) => s.name).sort()).toEqual(['bearerAuth', 'sessionCookie']);
	});
});
