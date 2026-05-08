import { test, expect } from '@playwright/test';
import { gotoDemoEditor } from './helpers/demo';

test.describe('Editor Interaction', () => {
	test.beforeEach(async ({ page }) => {
		await gotoDemoEditor(page);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
	});

	test('demo editor loads with toolbar and palette', async ({ page }) => {
		await expect(page.getByTestId('editor-toolbar')).toBeVisible();
		await expect(page.getByTestId('canvas-container')).toBeVisible();
		await expect(page.getByTestId('node-palette')).toBeVisible();
		await expect(page.getByTestId('toolbar-template-name')).toContainText('Invoice Processing Demo');
	});

	test('palette shows all node types', async ({ page }) => {
		const palette = page.getByTestId('node-palette');
		await expect(palette).toBeVisible();

		await expect(page.getByTestId('palette-item-start')).toBeVisible();
		await expect(page.getByTestId('palette-item-end')).toBeVisible();
		await expect(page.getByTestId('palette-item-human_task')).toBeVisible();
		await expect(page.getByTestId('palette-item-automated_step')).toBeVisible();
		await expect(page.getByTestId('palette-item-decision')).toBeVisible();
		await expect(page.getByTestId('palette-item-parallel_split')).toBeVisible();
		await expect(page.getByTestId('palette-item-parallel_join')).toBeVisible();
		await expect(page.getByTestId('palette-item-loop')).toBeVisible();
	});

	test('palette items are draggable', async ({ page }) => {
		const humanTaskItem = page.getByTestId('palette-item-human_task');
		await expect(humanTaskItem).toHaveAttribute('draggable', 'true');

		const automatedStepItem = page.getByTestId('palette-item-automated_step');
		await expect(automatedStepItem).toHaveAttribute('draggable', 'true');
	});

	test('drag node from palette to canvas creates node', async ({ page }) => {
		const dropZone = page.getByTestId('canvas-drop-zone');
		const humanTaskPalette = page.getByTestId('palette-item-human_task');

		// Perform drag and drop
		const dropBounds = await dropZone.boundingBox();
		if (!dropBounds) throw new Error('Drop zone not visible');

		await humanTaskPalette.dragTo(dropZone, {
			targetPosition: {
				x: dropBounds.width / 2,
				y: dropBounds.height / 2
			}
		});

		// After drop, a human task node should appear in the flow
		// The SvelteFlow renders nodes inside the canvas area
		await expect(page.getByTestId('node-human-task')).toBeVisible({ timeout: 3000 });
	});

	test('clicking a node selects it and opens property panel', async ({ page }) => {
		// The demo page starts with Start and End nodes
		// We need to click on a node to select it
		const startNode = page.getByTestId('node-start');
		await expect(startNode).toBeVisible();

		// Click the start node to select it
		await startNode.click();

		// Property panel should open for the selected node
		await expect(page.getByTestId('node-property-panel')).toBeVisible({ timeout: 3000 });
		await expect(page.getByTestId('input-node-label')).toBeVisible();
	});

	test('property panel shows label input that can be edited', async ({ page }) => {
		// Click start node to select it
		await page.getByTestId('node-start').click();
		await expect(page.getByTestId('node-property-panel')).toBeVisible({ timeout: 3000 });

		const labelInput = page.getByTestId('input-node-label');
		await expect(labelInput).toBeVisible();

		// The start node label should be "Start"
		await expect(labelInput).toHaveValue('Start');

		// Edit the label
		await labelInput.clear();
		await labelInput.fill('Begin');
		await expect(labelInput).toHaveValue('Begin');
	});

	test('close button dismisses property panel', async ({ page }) => {
		// Select a node first
		await page.getByTestId('node-start').click();
		await expect(page.getByTestId('node-property-panel')).toBeVisible({ timeout: 3000 });

		// Click the close button
		await page.getByTestId('btn-close-properties').click();
		await expect(page.getByTestId('node-property-panel')).not.toBeVisible();
	});

	test('toolbar buttons are present', async ({ page }) => {
		await expect(page.getByTestId('btn-preview-air')).toBeVisible();
		await expect(page.getByTestId('btn-save')).toBeVisible();
		await expect(page.getByTestId('btn-publish')).toBeVisible();
	});

	test('toolbar template name shows correct value', async ({ page }) => {
		await expect(page.getByTestId('toolbar-template-name')).toContainText('Invoice Processing Demo');
	});
});
