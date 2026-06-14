import { test, expect, type Page } from '@playwright/test';
import { gotoDemoEditor } from './helpers/demo';

/**
 * Per-executor icons for built-in AutomatedStep backends.
 *
 * Each backend ships a distinct Lucide icon NAME server-side
 * (shared/backends/src/registry.rs → GET /api/v1/backends → `icon`); the
 * canvas card and the backend picker resolve it through
 * `$lib/editor/backend-icons`. `gotoDemoEditor` mocks only the template GET and
 * lets `/api/v1/node-types` + `/api/v1/backends` flow through to a real mekhan,
 * so this exercises the genuine server-driven mapping end-to-end.
 *
 * Brand backends render a vendored mark `<svg class="brand-icon brand-{slug} …">`
 * (python→`brand-python`); brand-less backends render a Lucide glyph
 * `<svg class="lucide lucide-{name} …">` (http→`lucide-globe`). Both are
 * assertable by class.
 */
async function dropAutomatedStep(page: Page) {
	const dropZone = page.getByTestId('canvas-drop-zone');
	const bounds = await dropZone.boundingBox();
	if (!bounds) throw new Error('drop zone not visible');
	await page.getByTestId('palette-item-automated_step').dragTo(dropZone, {
		targetPosition: { x: bounds.width / 2, y: bounds.height / 2 }
	});
	const node = page.getByTestId('node-automated-step');
	await expect(node).toBeVisible({ timeout: 5000 });
	return node;
}

test.describe('Automated step backend icon', () => {
	test.beforeEach(async ({ page }) => {
		await gotoDemoEditor(page);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
		await expect(page.getByTestId('canvas-drop-zone')).toBeVisible();
	});

	test('card shows the python brand mark on drop, swaps to the http glyph on backend change', async ({
		page
	}) => {
		const node = await dropAutomatedStep(page);

		// Default backend is python → the Python BRAND mark, NOT the old generic
		// Cpu. Proves the resolver→card wiring against the live registry.
		await expect(node.locator('svg.brand-python')).toBeVisible({ timeout: 10000 });
		await expect(node.locator('svg.lucide-cpu')).toHaveCount(0);

		// Switch the backend to HTTP via the property panel.
		await node.click();
		await expect(page.getByTestId('node-property-panel')).toBeVisible({ timeout: 3000 });
		await page.getByTestId('backend-type-trigger').click();
		await page.getByTestId('backend-option-http').click();

		// The card icon reacts: http has no brand, so the generic globe glyph now,
		// and the Python mark is gone.
		await expect(node.locator('svg.lucide-globe')).toBeVisible({ timeout: 10000 });
		await expect(node.locator('svg.brand-python')).toHaveCount(0);
	});
});
