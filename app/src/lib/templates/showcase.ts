import type { WorkflowGraph } from '$lib/types/editor';
import type { Template } from '$lib/api/client';
import { listTemplates, getTemplate, createTemplate } from '$lib/api/client';

export const SHOWCASE_TEMPLATE_NAME = 'Invoice Processing Demo';
export const SHOWCASE_TEMPLATE_DESCRIPTION =
	'Showcase workflow demonstrating human tasks, automated steps, decisions, parallel split/join, and scopes.';

/**
 * Pre-built "Invoice Processing" workflow demonstrating node types with scoping.
 *
 * Flow:
 *   Start
 *     → Human Task: Review Invoice
 *     → Automated Step: Extract Data (Python)
 *     → Decision: Amount > $5,000?
 *       ├── Yes → [Scope: High-Value Review]
 *       │         ├── Parallel Split
 *       │         │   ├── Human Task: Manager Approval
 *       │         │   └── Automated Step: Compliance Check (Docker)
 *       │         ├── Parallel Join
 *       │         └── End: Approved
 *       └── No  → End: Processed
 */
export const showcaseGraph: WorkflowGraph = {
	nodes: [
		// ── Scope: wraps the parallel high-value review section ──
		{
			id: 'scope-parallel',
			type: 'scope',
			position: { x: 1040, y: 10 },
			width: 900,
			height: 280,
			data: { type: 'scope', label: 'High-Value Review' }
		},

		// ── Row 1: Entry ──────────────────────────────────────
		{
			id: 'start',
			type: 'start',
			position: { x: 40, y: 280 },
			data: {
				type: 'start',
				label: 'Start',
				initial: {
					id: 'in',
					label: 'Input',
					fields: [
						{
							name: 'invoice_id',
							label: 'Invoice ID',
							kind: 'text',
							required: true
						}
					]
				}
			}
		},

		// ── Row 2: Review ─────────────────────────────────────
		{
			id: 'review',
			type: 'human_task',
			position: { x: 240, y: 250 },
			data: {
				type: 'human_task',
				label: 'Review Invoice',
				taskTitle: 'Review Incoming Invoice',
				instructionsMdsvex:
					'Please review the invoice details below and verify the information is correct before proceeding.',
				steps: [
					{
						id: 'step-verify',
						title: 'Verify Details',
						blocks: [
							{
								type: 'input',
								field: {
									name: 'vendor_name',
									label: 'Vendor Name',
									kind: 'text',
									required: true,
									placeholder: 'Enter vendor name'
								}
							},
							{
								type: 'input',
								field: {
									name: 'invoice_amount',
									label: 'Invoice Amount ($)',
									kind: 'number',
									required: true
								}
							},
							{
								type: 'input',
								field: {
									name: 'description',
									label: 'Line Item Description',
									kind: 'textarea',
									required: false,
									placeholder: 'Describe the goods or services'
								}
							}
						]
					},
					{
						id: 'step-confirm',
						title: 'Confirmation',
						blocks: [
							{
								type: 'input',
								field: {
									name: 'verified',
									label: 'I confirm this invoice is accurate',
									kind: 'checkbox',
									required: true
								}
							}
						]
					}
				]
			}
		},

		// ── Row 3: Extract ────────────────────────────────────
		{
			id: 'extract',
			type: 'automated_step',
			position: { x: 520, y: 250 },
			data: {
				type: 'automated_step',
				label: 'Extract Data',
				description: 'OCR + NLP extraction pipeline',
				executionSpec: {
					backendType: 'python',
					entrypoint: 'main.py',
					config: {
						python: 'python3',
						requirements: [],
						virtualenv: false,
						sdk: true,
						inherit_env: true,
						env: {}
					}
				}
			}
		},

		// ── Row 4: Decision ───────────────────────────────────
		{
			id: 'check-amount',
			type: 'decision',
			position: { x: 800, y: 255 },
			data: {
				type: 'decision',
				label: 'Amount Check',
				description: 'Route based on invoice total',
				conditions: [
					{
						edgeId: 'branch-high',
						label: 'High Value (> $5,000)',
						guard: 'input.invoice_amount > 5000'
					}
				],
				defaultBranch: 'default'
			}
		},

		// ── Upper path: High value (inside scope-parallel) ───
		{
			id: 'split',
			type: 'parallel_split',
			parentId: 'scope-parallel',
			position: { x: 40, y: 110 },
			data: { type: 'parallel_split', label: 'Dual Review' }
		},
		{
			id: 'manager-approval',
			type: 'human_task',
			parentId: 'scope-parallel',
			position: { x: 280, y: 30 },
			data: {
				type: 'human_task',
				label: 'Manager Approval',
				taskTitle: 'Approve High-Value Invoice',
				instructionsMdsvex: 'This invoice exceeds $5,000 and requires manager sign-off.',
				steps: [
					{
						id: 'step-decide',
						title: 'Decision',
						blocks: [
							{
								type: 'input',
								field: {
									name: 'decision',
									label: 'Approval Decision',
									kind: 'select',
									required: true,
									options: ['Approve', 'Reject', 'Request More Info']
								}
							},
							{
								type: 'input',
								field: {
									name: 'comments',
									label: 'Comments',
									kind: 'textarea',
									required: false,
									placeholder: 'Optional notes for the finance team'
								}
							},
							{
								type: 'input',
								field: {
									name: 'signature',
									label: 'Manager Signature',
									kind: 'signature',
									required: true
								}
							}
						]
					}
				]
			}
		},
		{
			id: 'compliance',
			type: 'automated_step',
			parentId: 'scope-parallel',
			position: { x: 280, y: 200 },
			data: {
				type: 'automated_step',
				label: 'Compliance Check',
				description: 'Sanctions & fraud screening',
				executionSpec: {
					backendType: 'python',
					entrypoint: 'main.py',
					config: {
						python: 'python3',
						requirements: [],
						virtualenv: false,
						sdk: true,
						inherit_env: true,
						env: {}
					}
				}
			}
		},
		{
			id: 'join',
			type: 'parallel_join',
			parentId: 'scope-parallel',
			position: { x: 560, y: 110 },
			data: { type: 'parallel_join', label: 'Merge Results' }
		},
		{
			id: 'end-approved',
			type: 'end',
			parentId: 'scope-parallel',
			position: { x: 770, y: 110 },
			data: { type: 'end', label: 'Approved' }
		},

		// ── Lower path: Low value ─────────────────────────────
		{
			id: 'end-processed',
			type: 'end',
			position: { x: 1080, y: 410 },
			data: { type: 'end', label: 'Processed' }
		}
	],

	edges: [
		// Start → Review
		{
			id: 'e-start-review',
			source: 'start',
			target: 'review',
			type: 'sequence'
		},
		// Review → Extract
		{
			id: 'e-review-extract',
			source: 'review',
			target: 'extract',
			type: 'sequence'
		},
		// Extract → Decision
		{
			id: 'e-extract-decision',
			source: 'extract',
			target: 'check-amount',
			type: 'sequence'
		},

		// Decision → Parallel Split (high value)
		{
			id: 'e-decision-split',
			source: 'check-amount',
			target: 'split',
			sourceHandle: 'branch-high',
			label: '> $5,000',
			type: 'conditional'
		},
		// Decision → End (default / low value)
		{
			id: 'e-decision-processed',
			source: 'check-amount',
			target: 'end-processed',
			sourceHandle: 'default',
			label: '≤ $5,000',
			type: 'conditional'
		},

		// Split → Manager Approval
		{
			id: 'e-split-manager',
			source: 'split',
			target: 'manager-approval',
			type: 'sequence'
		},
		// Split → Compliance
		{
			id: 'e-split-compliance',
			source: 'split',
			target: 'compliance',
			type: 'sequence'
		},

		// Manager → Join
		{
			id: 'e-manager-join',
			source: 'manager-approval',
			target: 'join',
			type: 'sequence'
		},
		// Compliance → Join
		{
			id: 'e-compliance-join',
			source: 'compliance',
			target: 'join',
			type: 'sequence'
		},

		// Join → End (Approved)
		{
			id: 'e-join-end',
			source: 'join',
			target: 'end-approved',
			type: 'sequence'
		}
	]
};

/**
 * Inline `main.py` contents for each automated_step node. Seeded into the
 * Y.Doc at template creation so the demo lands publishable without the user
 * having to open the IDE and type a script first.
 *
 * The runner reads upstream token data from `inputs["input.json"]` (see
 * `engine/sdk/...` and the prepare-transition snapshot in the compiler).
 */
const showcaseFiles: Record<string, Record<string, string>> = {
	extract: {
		'main.py':
			'import json, os\n' +
			'\n' +
			'with open(os.path.join(os.environ["AITHERICON_INPUTS_DIR"], "input.json")) as f:\n' +
			'    data = json.load(f)\n' +
			'\n' +
			'result = {\n' +
			'    "vendor": data.get("vendor_name", ""),\n' +
			'    "amount": data.get("invoice_amount", 0),\n' +
			'    "extracted": True,\n' +
			'}\n' +
			'print(json.dumps(result))\n'
	},
	compliance: {
		'main.py':
			'import json, os\n' +
			'\n' +
			'with open(os.path.join(os.environ["AITHERICON_INPUTS_DIR"], "input.json")) as f:\n' +
			'    data = json.load(f)\n' +
			'\n' +
			'result = {\n' +
			'    "compliant": True,\n' +
			'    "risk_score": 0.12,\n' +
			'    "checked_at": "2024-01-01",\n' +
			'}\n' +
			'print(json.dumps(result))\n'
	}
};

/**
 * Find the singleton "Invoice Processing Demo" template, creating it on first use.
 * The demo entry point uses this so every visit lands on the same shared template.
 */
export async function findOrCreateShowcaseTemplate(): Promise<Template> {
	const existing = await listTemplates(1, 50, SHOWCASE_TEMPLATE_NAME);
	const match = existing.items.find((t) => t.name === SHOWCASE_TEMPLATE_NAME);
	if (match) {
		return getTemplate(match.id);
	}
	return createTemplate({
		name: SHOWCASE_TEMPLATE_NAME,
		description: SHOWCASE_TEMPLATE_DESCRIPTION,
		graph: showcaseGraph,
		files: showcaseFiles
	});
}
