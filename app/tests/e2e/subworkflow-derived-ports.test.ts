// SubWorkflow fixed/derived-ports regression test.
//
// Guards the authoring contract introduced when SubWorkflow ports stopped
// being hand-authored:
//   1) the output port is rendered READ-ONLY (DerivedPortsSection, "CHILD END"
//      badge) — there is no "Add field" affordance,
//   2) the input mapping renders one FIXED row per child Start field (target
//      field locked, only the expression editable),
//   3) both are sourced from GET /api/v1/templates/{id}/io-contract, the same
//      derivation the publish path freezes.
//
// Backend is mocked (unit-style e2e): listTemplates returns one published
// child, and the io-contract endpoint returns a known {input, output}.
import { test, expect, type Page } from '@playwright/test';

const CHILD_ID = '11111111-1111-1111-1111-111111111111';
const TEMPLATE_ID = 'demo-template-test';

// Minimal editor shell — a single Start node. We drop a fresh SubWorkflow node
// in the test, so the seeded graph content is irrelevant; this just avoids the
// disk-read demo helper (whose `__dirname` use breaks under ESM here).
const MINIMAL_GRAPH = {
	nodes: [
		{
			id: 'start',
			type: 'start',
			position: { x: 0, y: 0 },
			data: { type: 'start', label: 'Start', initial: { id: 'in', label: 'Input', fields: [] } }
		}
	],
	edges: []
};

async function gotoEditor(page: Page) {
	await page.route('**/api/v1/templates/' + TEMPLATE_ID, async (route) => {
		if (route.request().method() === 'GET') {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					id: TEMPLATE_ID,
					name: 'Parent Workflow',
					description: '',
					graph: MINIMAL_GRAPH,
					version: 1,
					published: false,
					author_id: '00000000-0000-0000-0000-000000000000',
					created_at: new Date().toISOString(),
					updated_at: new Date().toISOString()
				})
			});
		} else {
			await route.continue();
		}
	});
	await page.goto(`/templates/${TEMPLATE_ID}`);
}

// Child contract: input = {message:text}; output = {invoice_amount, status}
// (Json — result_mapping carries no kind).
const IO_CONTRACT = {
	input: {
		id: 'in',
		label: 'Input',
		fields: [{ name: 'message', label: 'message', kind: 'text', required: false }]
	},
	output: {
		id: 'out',
		label: 'Result',
		fields: [
			{ name: 'invoice_amount', label: 'invoice_amount', kind: 'json', required: false },
			{ name: 'status', label: 'status', kind: 'json', required: false }
		]
	}
};

async function mockBackend(page: Page) {
	// Empty scope is fine — the input rows render their own expression input
	// and field label regardless of RefPicker scope.
	await page.route('**/api/v1/analyze', async (route) => {
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ graph_ok: true, diagnostics: [], scopes: {} })
		});
	});

	// listTemplates(published=true) → one selectable child family.
	await page.route(/\/api\/v1\/templates(\?|$)/, async (route) => {
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({
				items: [
					{
						id: CHILD_ID,
						base_template_id: null,
						name: 'Child Workflow',
						description: '',
						visibility: 'public',
						published: true,
						version: 1,
						author_id: '00000000-0000-0000-0000-000000000000',
						created_at: new Date().toISOString(),
						updated_at: new Date().toISOString()
					}
				],
				total: 1,
				page: 1,
				page_size: 100
			})
		});
	});

	// The derived contract endpoint (registered last → highest priority).
	await page.route(/\/io-contract/, async (route) => {
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(IO_CONTRACT)
		});
	});
}

test.describe('SubWorkflow derived ports', () => {
	test.beforeEach(async ({ page }) => {
		await mockBackend(page);
		await gotoEditor(page);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
	});

	test('input rows are fixed to the child Start fields and the output is read-only derived', async ({
		page
	}) => {
		// Drop a fresh SubWorkflow node and open its panel.
		const dropZone = page.getByTestId('canvas-drop-zone');
		const palette = page.getByTestId('palette-item-sub_workflow');
		const bounds = await dropZone.boundingBox();
		if (!bounds) throw new Error('Drop zone not visible');
		await palette.dragTo(dropZone, {
			targetPosition: { x: bounds.width / 2, y: bounds.height / 2 }
		});

		const node = page.getByTestId('node-sub-workflow').first();
		await expect(node).toBeVisible({ timeout: 3000 });
		await node.click();

		const panel = page.getByTestId('node-property-panel');
		await expect(panel).toBeVisible();

		// Pick the child template via the browser dialog — this triggers the
		// io-contract fetch. The template picker is now a button that opens a
		// ChildWorkflowBrowser modal; each result is a `browser-pick-<familyId>`
		// button. A v1 row (base_template_id: null) has familyId === its own id.
		await panel.getByTestId('btn-open-subworkflow-browser').click();
		await page.getByTestId(`browser-pick-${CHILD_ID}`).click();

		// (1) Input rows are FIXED to the child's Start field(s).
		const inputField = panel.getByTestId('subworkflow-input-field');
		await expect(inputField).toHaveText('message', { timeout: 5000 });
		// The expression input exists; there is no free-text "child field" input
		// (target field is locked).
		await expect(panel.getByTestId('input-subworkflow-map-expr')).toBeVisible();
		await expect(panel.getByTestId('input-subworkflow-map-field')).toHaveCount(0);

		// (2) Output is read-only derived: the DerivedPortsSection "Child End"
		// badge (CSS-uppercased to "CHILD END" visually; DOM text is "Child End")
		// and the derived field names are shown, with NO add-field button.
		await expect(panel.getByText('Child End')).toBeVisible();
		await expect(panel.getByText('invoice_amount')).toBeVisible();
		await expect(panel.getByText('status')).toBeVisible();
		await expect(panel.getByRole('button', { name: /add field/i })).toHaveCount(0);
	});
});
