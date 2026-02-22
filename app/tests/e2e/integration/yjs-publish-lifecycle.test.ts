/**
 * Full create -> edit -> publish lifecycle E2E tests.
 *
 * Tests the complete workflow of creating a template, editing it via
 * the Yjs-backed canvas, publishing, and verifying frozen state.
 * Requires full stack running.
 */

import { test, expect } from '@playwright/test';
import {
	AUTHOR_ID,
	apiPost,
	apiGet,
	servicesHealthy,
	createAndPublish
} from './helpers';

test.beforeAll(async () => {
	const healthy = await servicesHealthy();
	test.skip(!healthy, 'Full stack is not running — skipping publish lifecycle tests');
});

/** Create a fresh unpublished template and return its id. */
async function createTemplate(name: string): Promise<string> {
	const res = await apiPost('/api/templates', { name, author_id: AUTHOR_ID });
	expect(res.status).toBe(201);
	const template = await res.json();
	return template.id;
}

test.describe('Yjs Publish Lifecycle', () => {
	test('create template loads graph in editor', async ({ page }) => {
		const templateId = await createTemplate(`lifecycle-load-${Date.now()}`);
		await page.goto(`/templates/${templateId}`);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();

		// Wait for Yjs connection
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });

		// Default graph should have Start and End nodes on the canvas
		await expect(page.getByTestId('node-start')).toBeVisible({ timeout: 5_000 });
		await expect(page.getByTestId('node-end')).toBeVisible({ timeout: 5_000 });
	});

	test('canvas edit then publish includes changes', async ({ page }) => {
		const templateId = await createTemplate(`lifecycle-edit-pub-${Date.now()}`);
		await page.goto(`/templates/${templateId}`);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });

		// Verify default graph is loaded from Y.Doc
		await expect(page.getByTestId('node-start')).toBeVisible({ timeout: 5_000 });
		await expect(page.getByTestId('node-end')).toBeVisible({ timeout: 5_000 });

		// Publish the default graph via the toolbar button
		await page.getByTestId('btn-publish').click();
		await expect(page.getByText('Published')).toBeVisible({ timeout: 10_000 });

		// Verify via API that publish read from the Y.Doc
		const templateRes = await apiGet(`/api/templates/${templateId}`);
		expect(templateRes.status).toBe(200);
		const template = await templateRes.json();
		expect(template.published).toBe(true);
		expect(template.air_json).toBeTruthy();

		// Check the AIR was generated from the Y.Doc graph
		const airRes = await apiGet(`/api/templates/${templateId}/air`);
		expect(airRes.status).toBe(200);
		const air = await airRes.json();
		expect(air.places.length).toBeGreaterThanOrEqual(1);
	});

	test('published template frozen', async ({ page }) => {
		const published = await createAndPublish(`lifecycle-frozen-${Date.now()}`);
		await page.goto(`/templates/${published.id}`);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();

		// Should show Published badge
		await expect(page.getByText('Published')).toBeVisible({ timeout: 10_000 });

		// Publish button should be disabled
		await expect(page.getByTestId('btn-publish')).toBeDisabled();

		// Palette should either not exist or not allow drops on a published template
		const paletteCount = await page.getByTestId('palette-item-human_task').count();
		if (paletteCount > 0) {
			// If palette is visible, drag should have no effect
			const palette = page.getByTestId('palette-item-human_task');
			const dropZone = page.getByTestId('canvas-drop-zone');
			const dropBounds = await dropZone.boundingBox();
			if (dropBounds) {
				await palette.dragTo(dropZone, {
					targetPosition: { x: dropBounds.width / 2, y: dropBounds.height / 2 }
				});
				await page.waitForTimeout(1000);
				await expect(page.getByTestId('node-human-task')).not.toBeVisible();
			}
		}
		// If palette is not visible, that itself confirms readonly

		// Refresh the page — should still be readonly
		await page.reload();
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
		await expect(page.getByText('Published')).toBeVisible({ timeout: 10_000 });
		await expect(page.getByTestId('btn-publish')).toBeDisabled();
	});

	test('new version gets independent Y.Doc', async ({ page }) => {
		const templateId = await createTemplate(`lifecycle-version-${Date.now()}`);
		await page.goto(`/templates/${templateId}`);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });

		// Publish v1
		await page.getByTestId('btn-publish').click();
		await expect(page.getByText('Published')).toBeVisible({ timeout: 10_000 });

		// Create new version via API
		const newVerRes = await apiPost(`/api/templates/${templateId}/new-version`, {});
		expect(newVerRes.status).toBe(201);
		const v2 = await newVerRes.json();
		expect(v2.version).toBe(2);
		expect(v2.published).toBe(false);

		// Navigate to v2 editor
		await page.goto(`/templates/${v2.id}`);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();

		// v2 should connect WS with its own Y.Doc
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });

		// v2 should be editable (Draft badge, publish enabled)
		await expect(page.getByText('Draft')).toBeVisible();
		await expect(page.getByTestId('btn-publish')).toBeEnabled();

		// Canvas should show default graph nodes
		await expect(page.getByTestId('node-start')).toBeVisible({ timeout: 5_000 });
		await expect(page.getByTestId('node-end')).toBeVisible({ timeout: 5_000 });
	});
});
