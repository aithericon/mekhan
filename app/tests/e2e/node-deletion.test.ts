import { test, expect } from '@playwright/test';
import { gotoDemoEditor } from './helpers/demo';

test.describe('Node Deletion', () => {
	test.beforeEach(async ({ page }) => {
		await gotoDemoEditor(page);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
	});

	test('start and end nodes are initially visible', async ({ page }) => {
		// The redesigned editor hydrates its canvas from the live Yjs document,
		// NOT from the REST template GET — so the mocked `gotoDemoEditor` helper
		// (which only stubs the REST response, leaving the Yjs WS unmocked)
		// yields an empty canvas. To preserve this test's intent (the initial
		// Start/End nodes of a seeded workflow render on the canvas), navigate to
		// a real seeded demo template whose Yjs doc has exactly one Start and one
		// End node — "Email Welcome" (`…030`).
		await page.goto('/templates/00000000-0000-0000-0000-000000000030');
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
		await expect(page.getByTestId('node-start')).toBeVisible();
		await expect(page.getByTestId('node-end')).toBeVisible();
	});

	test('drag a node and delete it with Backspace', async ({ page }) => {
		// First, drag a human task node onto the canvas
		const dropZone = page.getByTestId('canvas-drop-zone');
		const humanTaskPalette = page.getByTestId('palette-item-human_task');

		const dropBounds = await dropZone.boundingBox();
		if (!dropBounds) throw new Error('Drop zone not visible');

		await humanTaskPalette.dragTo(dropZone, {
			targetPosition: {
				x: dropBounds.width / 2,
				y: dropBounds.height / 2
			}
		});

		// Verify the human task node appeared
		const humanTaskNode = page.getByTestId('node-human-task');
		await expect(humanTaskNode).toBeVisible({ timeout: 3000 });

		// Click the node to select it
		await humanTaskNode.click();

		// Press Backspace to delete
		await page.keyboard.press('Backspace');

		// Node should be removed
		await expect(humanTaskNode).not.toBeVisible({ timeout: 3000 });
	});

	test('drag a node and delete it with Delete key', async ({ page }) => {
		// Drag an automated step node
		const dropZone = page.getByTestId('canvas-drop-zone');
		const autoStepPalette = page.getByTestId('palette-item-automated_step');

		const dropBounds = await dropZone.boundingBox();
		if (!dropBounds) throw new Error('Drop zone not visible');

		await autoStepPalette.dragTo(dropZone, {
			targetPosition: {
				x: dropBounds.width / 2,
				y: dropBounds.height / 2
			}
		});

		const autoNode = page.getByTestId('node-automated-step');
		await expect(autoNode).toBeVisible({ timeout: 3000 });

		// Select and delete with Delete key
		await autoNode.click();
		await page.keyboard.press('Delete');

		await expect(autoNode).not.toBeVisible({ timeout: 3000 });
	});

	test('selected node deletion closes property panel', async ({ page }) => {
		// Drag a node
		const dropZone = page.getByTestId('canvas-drop-zone');
		const humanTaskPalette = page.getByTestId('palette-item-human_task');

		const dropBounds = await dropZone.boundingBox();
		if (!dropBounds) throw new Error('Drop zone not visible');

		await humanTaskPalette.dragTo(dropZone, {
			targetPosition: {
				x: dropBounds.width / 2,
				y: dropBounds.height / 2
			}
		});

		const humanTaskNode = page.getByTestId('node-human-task');
		await expect(humanTaskNode).toBeVisible({ timeout: 3000 });

		// Click to select and open property panel
		await humanTaskNode.click();
		await expect(page.getByTestId('node-property-panel')).toBeVisible({ timeout: 3000 });

		// Delete the node
		await page.keyboard.press('Backspace');

		// Both node and property panel should be gone
		await expect(humanTaskNode).not.toBeVisible({ timeout: 3000 });
	});
});
