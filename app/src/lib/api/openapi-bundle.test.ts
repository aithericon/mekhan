import { describe, it, expect } from 'vitest';
import {
	parseBundle,
	type ManualEndpoint,
	type WebhookEndpoint,
	type RunTemplateEndpoint
} from './openapi-bundle';

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

// A run-by-template op (POST /api/v1/instances#tpl=<id>) for a trigger-less
// template: the start_tokens contract is typed from the Start block port.
const RUN_DOC = {
	openapi: '3.0.3',
	info: { title: 'Folder: Basics' },
	paths: {
		'/api/v1/instances#tpl=00000000-0000-0000-0000-000000000042': {
			post: {
				tags: ['templates', 'Hello World'],
				summary: 'Run Hello World',
				security: [{ sessionCookie: [] }, { bearerAuth: [] }],
				'x-mekhan-run-template': true,
				'x-mekhan-template-id': '00000000-0000-0000-0000-000000000042',
				requestBody: {
					content: {
						'application/json': { schema: { $ref: '#/components/schemas/RunTemplate_x_Request' } }
					}
				}
			}
		}
	},
	components: {
		schemas: {
			RunTemplate_x_Request: {
				type: 'object',
				properties: {
					template_id: { type: 'string', enum: ['00000000-0000-0000-0000-000000000042'] },
					start_tokens: {
						type: 'array',
						minItems: 1,
						maxItems: 1,
						items: {
							type: 'object',
							properties: {
								start_block_id: { type: 'string', enum: ['start_main'] },
								token: {
									type: 'object',
									properties: {
										subject: { type: 'string' },
										count: { type: 'number' }
									},
									required: ['subject']
								}
							}
						}
					}
				},
				required: ['template_id', 'start_tokens']
			}
		}
	}
};

describe('parseBundle run-template', () => {
	it('parses a run-by-template op into a typed start-block contract', () => {
		const parsed = parseBundle(RUN_DOC);
		const run = parsed.endpoints.find((e) => e.kind === 'run') as RunTemplateEndpoint;
		expect(run).toBeTruthy();
		expect(run.templateId).toBe('00000000-0000-0000-0000-000000000042');
		expect(run.title).toBe('Run Hello World');
		expect(run.templateName).toBe('Hello World');
		expect(run.security).toEqual(['sessionCookie', 'bearerAuth']);
		expect(run.startBlocks).toHaveLength(1);
		const sb = run.startBlocks[0];
		expect(sb.startBlockId).toBe('start_main');
		const byName = Object.fromEntries(sb.fields.map((f) => [f.name, f]));
		expect(byName.subject.type).toBe('string');
		expect(byName.subject.required).toBe(true);
		expect(byName.count.type).toBe('number');
		expect(byName.count.required).toBe(false);
	});
});
