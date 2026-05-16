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
				// Registers a named HPI process per instance — the process list
				// shows "Invoice <id>" instead of an unnamed/auto-discovered row.
				processName: 'Invoice {{ invoice_id }}',
				initial: {
					id: 'in',
					label: 'Invoice Intake',
					fields: [
						{
							name: 'invoice_file',
							label: 'Invoice Image (PNG, JPG, or WebP)',
							kind: 'file',
							required: true,
							accept: 'image/png,image/jpeg,image/webp'
						},
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
								type: 'image',
								url: '{{ invoice_file.url }}',
								alt: 'Uploaded invoice',
								caption: 'Original invoice document (uploaded at instance start)'
							},
							{
								type: 'download',
								downloads: [
									{
										url: '{{ invoice_file.url }}',
										filename: '{{ invoice_file.filename }}',
										mime_type: '{{ invoice_file.content_type }}',
										description: 'Original uploaded invoice'
									}
								]
							},
							{ type: 'divider' },
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
				},
				// Declared data contract so the inspector shows what this step
				// emits. `input` is left pass-through (empty fields) — the edge
				// from Review carries the full form token, more fields than the
				// step reads, which an explicit subset would reject at compile.
				// `output` mirrors the keys main.py prints; downstream the
				// Decision branches on `input.invoice_amount`, which still
				// resolves transitively from the Review human task's output.
				output: {
					id: 'out',
					label: 'Extracted',
					fields: [
						{ name: 'vendor', label: 'Vendor', kind: 'text', required: true },
						{ name: 'amount', label: 'Amount', kind: 'number', required: true },
						{ name: 'extracted', label: 'Extracted', kind: 'bool', required: true }
					]
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
				},
				// Matches the set_output() calls in this node's main.py.
				output: {
					id: 'out',
					label: 'Screening result',
					fields: [
						{ name: 'compliant', label: 'Compliant', kind: 'bool', required: true },
						{ name: 'risk_score', label: 'Risk Score', kind: 'number', required: true },
						{ name: 'checked_at', label: 'Checked At', kind: 'text', required: true }
					]
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
			targetHandle: 'in',
			type: 'sequence'
		},
		// Review → Extract
		{
			id: 'e-review-extract',
			source: 'review',
			target: 'extract',
			targetHandle: 'in',
			type: 'sequence'
		},
		// Extract → Decision
		{
			id: 'e-extract-decision',
			source: 'extract',
			target: 'check-amount',
			targetHandle: 'in',
			type: 'sequence'
		},

		// Decision → Parallel Split (high value)
		{
			id: 'e-decision-split',
			source: 'check-amount',
			target: 'split',
			sourceHandle: 'branch-high',
			targetHandle: 'in',
			label: '> $5,000',
			type: 'conditional'
		},
		// Decision → End (default / low value)
		{
			id: 'e-decision-processed',
			source: 'check-amount',
			target: 'end-processed',
			sourceHandle: 'default',
			targetHandle: 'in',
			label: '≤ $5,000',
			type: 'conditional'
		},

		// Split → Manager Approval
		{
			id: 'e-split-manager',
			source: 'split',
			target: 'manager-approval',
			targetHandle: 'in',
			type: 'sequence'
		},
		// Split → Compliance
		{
			id: 'e-split-compliance',
			source: 'split',
			target: 'compliance',
			targetHandle: 'in',
			type: 'sequence'
		},

		// Manager → Join
		{
			id: 'e-manager-join',
			source: 'manager-approval',
			target: 'join',
			targetHandle: 'in',
			type: 'sequence'
		},
		// Compliance → Join
		{
			id: 'e-compliance-join',
			source: 'compliance',
			target: 'join',
			targetHandle: 'in',
			type: 'sequence'
		},

		// Join → End (Approved)
		{
			id: 'e-join-end',
			source: 'join',
			target: 'end-approved',
			targetHandle: 'in',
			type: 'sequence'
		}
	]
};

/**
 * Inline `main.py` contents for each automated_step node. Seeded into the
 * Y.Doc at template creation so the demo lands publishable without the user
 * having to open the IDE and type a script first.
 *
 * These use the Aithericon Python SDK the way the runner intends: the runner
 * (executor PythonBackend) auto-imports the SDK, calls `aithericon.init()`
 * before the user code and `aithericon.shutdown()` after, and injects
 * `inputs`, `set_output`, and `log_*` into scope. Step code therefore just
 * calls those helpers directly — it must NOT re-run init / ExecutionContext,
 * which would double the IPC lifecycle. The upstream token is the staged
 * `input.json` (the compiler's prepare-transition snapshot); each emitted
 * `set_output(name, value)` becomes a field on the node's declared output
 * port.
 */
const showcaseFiles: Record<string, Record<string, string>> = {
	extract: {
		'main.py': `# Extract Data — OCR + NLP extraction (Aithericon Python backend).
#
# The SDK runner injects these into scope (no import / init / shutdown):
#   set_output, log_info/log_warn/log_error/log_debug, log_metric,
#   define_phases, update_phase, update_progress.
# 'define_phases' declares the process layout the user watches live; each
# 'update_phase' / 'update_progress' / 'log_*' call streams to the process
# view via the executor → causality → hpi_logs/hpi_metrics pipeline.
# 'load_input()' returns the workflow token; the generated _aithericon_io.pyi
# types this node's fields so 'token.vendor_name' autocompletes.
import time

from _aithericon_io import load_input

token = load_input()
vendor = token.vendor_name or ""
amount = token.invoice_amount or 0

# Process layout / definition surfaced to the user for this step.
define_phases(["Load document", "OCR scan", "NLP extraction", "Validate", "Emit"])

update_phase("Load document", "running")
log_info("loading invoice token", vendor=vendor, amount=amount)
update_progress(0.05, "Reading workflow token")
time.sleep(0.4)  # demo pacing so the live phase/progress stream is visible
update_phase("Load document", "completed")

update_phase("OCR scan", "running")
log_info("running OCR over the uploaded invoice image")
update_progress(0.3, "OCR scan in progress")
time.sleep(0.6)
log_info("OCR finished", pages=1, confidence=0.97)
log_metric("ocr_confidence", 0.97)
update_phase("OCR scan", "completed")

update_phase("NLP extraction", "running")
log_info("extracting structured fields: vendor, amount, line items")
update_progress(0.6, "NLP field extraction")
time.sleep(0.6)
update_phase("NLP extraction", "completed")

update_phase("Validate", "running")
if amount <= 0:
    log_warn("extracted amount is non-positive — downstream review advised", amount=amount)
else:
    log_info("amount sanity check passed", amount=amount)
update_progress(0.85, "Validating extracted fields")
time.sleep(0.3)
update_phase("Validate", "completed")

update_phase("Emit", "running")
set_output("vendor", vendor)
set_output("amount", amount)
set_output("extracted", True)
log_metric("invoice_amount", float(amount))
log_info("extraction complete", vendor=vendor, amount=amount)
update_progress(1.0, "Extraction done")
update_phase("Emit", "completed")
`
	},
	compliance: {
		'main.py': `# Compliance Check — sanctions & fraud screening (Python backend).
#
# Same injected SDK handler as Extract: define_phases declares the process
# layout the user sees; update_phase/update_progress/log_*/log_metric stream
# live to the process view. 'token' is the accumulated workflow token; its
# per-node field types come from the generated _aithericon_io.pyi (upstream
# form + Extract output).
import time

from _aithericon_io import load_input

token = load_input()
amount = token.amount if token.amount is not None else (token.invoice_amount or 0)

# Process layout / definition surfaced to the user for this step.
define_phases(["Load token", "Sanctions screening", "Fraud scoring", "Decision"])

update_phase("Load token", "running")
log_info("starting compliance screening", amount=amount)
update_progress(0.1, "Loading accumulated token")
time.sleep(0.3)  # demo pacing so the live phase/progress stream is visible
update_phase("Load token", "completed")

update_phase("Sanctions screening", "running")
log_info("checking vendor against sanctions / watch lists")
update_progress(0.4, "Sanctions list lookup")
time.sleep(0.6)
log_info("no sanctions match found")
update_phase("Sanctions screening", "completed")

update_phase("Fraud scoring", "running")
log_info("scoring fraud risk", model="rules-v2", amount=amount)
update_progress(0.75, "Running fraud risk model")
time.sleep(0.5)
risk_score = 0.12
log_metric("risk_score", risk_score)
update_phase("Fraud scoring", "completed")

update_phase("Decision", "running")
compliant = risk_score < 0.5
if not compliant:
    log_warn("invoice flagged as HIGH RISK — routing to manual review", risk_score=risk_score)
else:
    log_info("invoice cleared compliance", risk_score=risk_score)
update_progress(1.0, "Compliance complete")
update_phase("Decision", "completed")

set_output("compliant", compliant)
set_output("risk_score", risk_score)
set_output("checked_at", "2024-01-01")
`
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
