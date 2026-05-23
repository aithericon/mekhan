import { test, expect, type Locator, type Page } from '@playwright/test';
import { gotoDemoEditor } from './helpers/demo';

// The showcase graph is mocked into the template GET response but the
// consolidated editor hydrates `binding.graph` from the Yjs Y.Doc, not the
// REST payload — and these tests run without a Yjs WS backend. So the canvas
// starts empty and any test that needs a node clicks one it drops itself.
async function dropHumanTask(page: Page): Promise<Locator> {
	const dropZone = page.getByTestId('canvas-drop-zone');
	const dropBounds = await dropZone.boundingBox();
	if (!dropBounds) throw new Error('Drop zone not visible');
	await page.getByTestId('palette-item-human_task').dragTo(dropZone, {
		targetPosition: { x: dropBounds.width / 2, y: dropBounds.height / 2 }
	});
	const node = page.getByTestId('node-human-task');
	await expect(node).toBeVisible({ timeout: 3000 });
	return node;
}

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
		const node = await dropHumanTask(page);
		await node.click();

		await expect(page.getByTestId('node-property-panel')).toBeVisible({ timeout: 3000 });
		await expect(page.getByTestId('input-node-label')).toBeVisible();
	});

	test('property panel shows label input that can be edited', async ({ page }) => {
		const node = await dropHumanTask(page);
		await node.click();
		await expect(page.getByTestId('node-property-panel')).toBeVisible({ timeout: 3000 });

		const labelInput = page.getByTestId('input-node-label');
		await expect(labelInput).toBeVisible();

		// Default label seeded by `createDefaultNodeData('human_task')`.
		await expect(labelInput).toHaveValue('Human Task');

		await labelInput.clear();
		await labelInput.fill('Review Invoice');
		await expect(labelInput).toHaveValue('Review Invoice');
	});

	test('close button dismisses property panel', async ({ page }) => {
		const node = await dropHumanTask(page);
		await node.click();
		await expect(page.getByTestId('node-property-panel')).toBeVisible({ timeout: 3000 });

		await page.getByTestId('btn-close-properties').click();
		await expect(page.getByTestId('node-property-panel')).not.toBeVisible();
	});

	test('toolbar buttons are present', async ({ page }) => {
		// `btn-save` is intentionally absent: the template editor saves via Yjs
		// sync, so the page never wires `onsave` and the toolbar's
		// `{#if onsave}` branch stays off.
		await expect(page.getByTestId('btn-preview-air')).toBeVisible();
		await expect(page.getByTestId('btn-publish')).toBeVisible();
	});

	test('toolbar template name shows correct value', async ({ page }) => {
		await expect(page.getByTestId('toolbar-template-name')).toContainText('Invoice Processing Demo');
	});
});
