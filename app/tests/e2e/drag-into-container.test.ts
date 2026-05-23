import { test, expect, type Page, type Locator } from '@playwright/test';
import { gotoDemoEditor } from './helpers/demo';

/**
 * Smoke test for the drag-into-container UX (Scope + Loop).
 *
 * Three things to prove, in order of strength:
 *  1. Dropping a node onto a container's bounds visually places it inside
 *     (weakest — could be coincidence, just position).
 *  2. The new node becomes a Svelte Flow child of the container in the DOM
 *     (medium — verified by parent-attribute walk).
 *  3. Dragging the container moves the child with it (strongest — proves
 *     real parent-relative coordinates and `extent: 'parent'` locking).
 *
 * We assert (3) directly because it's the only check that actually
 * distinguishes parenting from visual coincidence.
 */

async function dropFromPalette(
	page: Page,
	paletteTestId: string,
	target: Locator,
	pos: { x: number; y: number }
) {
	await page.getByTestId(paletteTestId).dragTo(target, { targetPosition: pos });
}

async function nodeBox(page: Page, testId: string): Promise<{ x: number; y: number; width: number; height: number }> {
	const box = await page.getByTestId(testId).boundingBox();
	if (!box) throw new Error(`Node ${testId} not visible`);
	return box;
}

test.describe('Drag into container — Loop', () => {
	test.beforeEach(async ({ page }) => {
		await gotoDemoEditor(page);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
		await expect(page.getByTestId('canvas-drop-zone')).toBeVisible();
	});

	test('dropping a Loop then a HumanTask inside it parents the HumanTask', async ({ page }) => {
		const dropZone = page.getByTestId('canvas-drop-zone');
		const dropBounds = await dropZone.boundingBox();
		if (!dropBounds) throw new Error('drop zone not visible');

		// Drop a Loop in a clear region (right side of the canvas — showcase's
		// existing nodes sit on the left).
		const loopDropPos = { x: dropBounds.width * 0.7, y: dropBounds.height * 0.5 };
		await dropFromPalette(page, 'palette-item-loop', dropZone, loopDropPos);
		const loop = page.getByTestId('node-loop').first();
		await expect(loop).toBeVisible({ timeout: 5000 });

		// Drop a HumanTask inside the Loop's visible bounds. Position relative
		// to the canvas-drop-zone, biased slightly off-center so we don't land
		// on the title row or the perimeter handles.
		const loopBoxBefore = await nodeBox(page, 'node-loop');
		const taskDropPos = {
			x: loopBoxBefore.x - dropBounds.x + loopBoxBefore.width / 2,
			y: loopBoxBefore.y - dropBounds.y + loopBoxBefore.height * 0.65
		};
		await dropFromPalette(page, 'palette-item-human_task', dropZone, taskDropPos);
		const task = page.getByTestId('node-human-task').last();
		await expect(task).toBeVisible({ timeout: 5000 });

		// Sanity: the new task is visually inside the Loop's bounds.
		const taskBox = await nodeBox(page, 'node-human-task');
		const taskCenter = { x: taskBox.x + taskBox.width / 2, y: taskBox.y + taskBox.height / 2 };
		expect(taskCenter.x).toBeGreaterThan(loopBoxBefore.x);
		expect(taskCenter.x).toBeLessThan(loopBoxBefore.x + loopBoxBefore.width);
		expect(taskCenter.y).toBeGreaterThan(loopBoxBefore.y);
		expect(taskCenter.y).toBeLessThan(loopBoxBefore.y + loopBoxBefore.height);

		// THE proof: drag the Loop, verify the task moves with it. If the
		// task were merely a visual neighbor (unparented), it would stay put.
		// Drag the Loop's title bar (top inside edge) — safer than the
		// container body which may forward events to children.
		const loopGrab = { x: loopBoxBefore.x + 60, y: loopBoxBefore.y + 12 };
		const delta = { x: -120, y: -80 };
		await page.mouse.move(loopGrab.x, loopGrab.y);
		await page.mouse.down();
		await page.mouse.move(loopGrab.x + delta.x, loopGrab.y + delta.y, { steps: 8 });
		await page.mouse.up();

		// Loop should have moved by ~delta. Allow a few px of slop for
		// snap-to-grid or pointer-event quirks.
		const loopBoxAfter = await nodeBox(page, 'node-loop');
		expect(loopBoxAfter.x).toBeLessThan(loopBoxBefore.x - 50);
		expect(loopBoxAfter.y).toBeLessThan(loopBoxBefore.y - 30);

		// Task should have moved by the same delta. This is the real
		// parenting proof — only true if Svelte Flow is rendering the task
		// with `parentId == loop.id` and `extent: 'parent'`.
		const taskBoxAfter = await nodeBox(page, 'node-human-task');
		const taskShift = {
			x: taskBoxAfter.x - taskBox.x,
			y: taskBoxAfter.y - taskBox.y
		};
		const loopShift = {
			x: loopBoxAfter.x - loopBoxBefore.x,
			y: loopBoxAfter.y - loopBoxBefore.y
		};
		// Allow ±5 px slop between the two shifts.
		expect(Math.abs(taskShift.x - loopShift.x)).toBeLessThan(5);
		expect(Math.abs(taskShift.y - loopShift.y)).toBeLessThan(5);
	});
});
