// Variable-selector (RefPicker / InsertRefButton) regression test.
//
// Three things this exercises:
//   1) NodePropertyPanel debounce-loads scope from POST /api/analyze and
//      hands it to the active section as `scope`.
//   2) The Decision branch editor's RefPicker shows the loaded scope.
//   3) PhaseUpdate's new InsertRefButton is rendered when scope is non-empty
//      — which is the new wiring this test guards against regressions for.
import { test, expect, type Page } from '@playwright/test';
import { gotoDemoEditor } from './helpers/demo';

/**
 * Synthetic /api/analyze response: enough scope for both the showcase's
 * `check-amount` Decision node and any newly-dropped node id. The picker
 * keys by node id, so we also map a wildcard `*` via a runtime injection
 * trick: every key in `scopes` gets the same entries.
 */
const ANALYZE_RESPONSE = {
	graph_ok: true,
	diagnostics: [],
	scopes: {
		'check-amount': [
			{
				producer_node: 'review',
				producer_label: 'Manual Review',
				path: 'review.invoice_amount',
				ty: 'Number',
				note: ''
			},
			{
				producer_node: 'review',
				producer_label: 'Manual Review',
				path: 'review.vendor_name',
				ty: 'String',
				note: ''
			}
		]
	}
};

async function mockAnalyze(page: Page) {
	await page.route('**/api/analyze', async (route) => {
		if (route.request().method() === 'POST') {
			// Read the body to discover *every* node id in the live graph,
			// then synthesize scope for all of them — saves us from chasing
			// freshly-generated ids when the user drops a new node.
			let nodeIds: string[] = [];
			try {
				const body = route.request().postDataJSON() as {
					graph?: { nodes?: { id: string }[] };
				};
				nodeIds = body.graph?.nodes?.map((n) => n.id) ?? [];
			} catch {
				// fall through to the static map
			}
			const scopes: Record<string, unknown[]> = { ...ANALYZE_RESPONSE.scopes };
			for (const id of nodeIds) {
				if (!scopes[id]) scopes[id] = ANALYZE_RESPONSE.scopes['check-amount'];
			}
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({ ...ANALYZE_RESPONSE, scopes })
			});
		} else {
			await route.continue();
		}
	});
}

test.describe('Variable picker (RefPicker / InsertRefButton)', () => {
	test.beforeEach(async ({ page }) => {
		await mockAnalyze(page);
		await gotoDemoEditor(page);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
	});

	test('Decision branch editor exposes RefPicker on each branch when scope loads', async ({
		page
	}) => {
		// Drop a fresh Decision (the showcase graph hydrates from the GET-template
		// mock only via the backend — without a Yjs server the live doc stays
		// empty, so we drag instead of clicking the showcase's check-amount).
		const dropZone = page.getByTestId('canvas-drop-zone');
		const decisionPalette = page.getByTestId('palette-item-decision');
		const dropBounds = await dropZone.boundingBox();
		if (!dropBounds) throw new Error('Drop zone not visible');
		await decisionPalette.dragTo(dropZone, {
			targetPosition: { x: dropBounds.width / 2, y: dropBounds.height / 2 }
		});

		const decisionNode = page.getByTestId('node-decision').first();
		await expect(decisionNode).toBeVisible({ timeout: 3000 });
		await decisionNode.click();

		const panel = page.getByTestId('node-property-panel');
		await expect(panel).toBeVisible();

		// A fresh Decision has no branches. Adding one mounts a GuardEditor with
		// a RefPicker placeholder "Pick field…" — visible only when scope > 0.
		const addBranch = panel.getByRole('button', { name: /add branch/i });
		await addBranch.click();

		await expect(panel.getByText('Pick field…').first()).toBeVisible({ timeout: 5000 });
	});

	test('PhaseUpdate sidebar exposes the InsertRefButton when scope is populated', async ({
		page
	}) => {
		// Drag a phase_update node onto the canvas.
		const dropZone = page.getByTestId('canvas-drop-zone');
		const phasePalette = page.getByTestId('palette-item-phase_update');
		const dropBounds = await dropZone.boundingBox();
		if (!dropBounds) throw new Error('Drop zone not visible');
		await phasePalette.dragTo(dropZone, {
			targetPosition: { x: dropBounds.width / 2, y: dropBounds.height / 2 }
		});

		// Click the freshly-placed node so its property panel opens.
		const phaseNode = page.getByTestId('node-phase-update').first();
		await expect(phaseNode).toBeVisible({ timeout: 3000 });
		await phaseNode.click();

		const panel = page.getByTestId('node-property-panel');
		await expect(panel).toBeVisible();

		// The new InsertRefButton's placeholder text — only renders when the
		// scope-loading effect has resolved and `scope.length > 0`.
		await expect(panel.getByText('Insert variable…').first()).toBeVisible({ timeout: 5000 });

		// The old "inbound token" hint is gone.
		await expect(panel.getByText('inbound token')).toHaveCount(0);
	});

	test('IDE banner surfaces when the analyzer says graph is not scopable', async ({ page }) => {
		// Re-route /api/analyze to return graph_ok:false with a synthetic
		// diagnostic, then navigate to the IDE side. The banner should be the
		// canonical "why is the picker empty?" affordance.
		await page.unroute('**/api/analyze');
		await page.route('**/api/analyze', async (route) => {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					graph_ok: false,
					diagnostics: [
						{ kind: 'graph_not_scopable', message: 'dangling edge from Start', node_id: 'start' }
					],
					scopes: {}
				})
			});
		});

		// Same demo template, /ide route. The IDE's $effect kicks off the
		// initial refreshScopes on mount; banner should appear shortly after.
		await page.goto('/templates/demo-template-test/ide');

		const banner = page.getByTestId('ide-analyzer-banner');
		await expect(banner).toBeVisible({ timeout: 5000 });
		await expect(banner).toContainText('Variable references unavailable');
		await expect(banner).toContainText('1 diagnostic');

		// Diagnostic detail is collapsible — open it and confirm the message.
		await banner.getByRole('group').or(banner.locator('details')).first().click();
		await expect(banner).toContainText('graph_not_scopable');
		await expect(banner).toContainText('dangling edge from Start');
	});

	test('PhaseUpdate hides RefPicker affordance when no scope is in scope', async ({ page }) => {
		// Mock /api/analyze to return empty scope for everything.
		await page.unroute('**/api/analyze');
		await page.route('**/api/analyze', async (route) => {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({ graph_ok: true, diagnostics: [], scopes: {} })
			});
		});

		const dropZone = page.getByTestId('canvas-drop-zone');
		const phasePalette = page.getByTestId('palette-item-phase_update');
		const dropBounds = await dropZone.boundingBox();
		if (!dropBounds) throw new Error('Drop zone not visible');
		await phasePalette.dragTo(dropZone, {
			targetPosition: { x: dropBounds.width / 2, y: dropBounds.height / 2 }
		});

		await page.getByTestId('node-phase-update').first().click();
		const panel = page.getByTestId('node-property-panel');
		await expect(panel).toBeVisible();

		// No scope → the "Insert variable…" trigger is conditionally hidden
		// by the `{#if scope.length > 0}` guard in PhaseUpdateNodeSection.
		await expect(panel.getByText('Insert variable…')).toHaveCount(0);
	});
});
