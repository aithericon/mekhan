import { test, expect, type Page, type Locator } from '@playwright/test';
import { showcaseGraph } from '../../src/lib/templates/showcase';

const DEMO_ID = 'demo-template-test';
const DEMO_NAME = 'Invoice Processing Demo';
const APP_URL = process.env.PLAYWRIGHT_BASE_URL ?? 'http://localhost:5173';

async function gotoDemoEditor(page: Page) {
	await page.route('**/api/templates/' + DEMO_ID, async (route) => {
		if (route.request().method() === 'GET') {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					id: DEMO_ID,
					name: DEMO_NAME,
					description: 'Showcase workflow',
					graph: showcaseGraph,
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
	await page.goto(`${APP_URL}/templates/${DEMO_ID}`);
}

/**
 * Resize-persistence smoke test for container nodes (Scope + Loop).
 *
 * Two assertions, in order of strength:
 *  1. DOM bounding box reflects the new size after the gesture (weakest —
 *     proves only that xyflow updated its local state).
 *  2. The Y.Doc carries the new width/height for the resized node (the
 *     strong proof — without `WorkflowCanvas.reportResize` →
 *     `binding.resizeNode` the gesture lives only in local state and is
 *     reverted on the next Y.Doc-driven re-sync. The dev-only `__yjs_doc`
 *     window handle lets the test read the Y.Map directly).
 *
 * Implementation notes:
 *  - We grab the **top-left** corner handle and drag it up-left. The
 *    bottom-right corner falls under the property panel that opens on
 *    selection; the bottom-left corner overlaps the minimap. The top-left
 *    is the only corner reliably free of overlay UI under `fitView`'s
 *    auto-zoom.
 *  - Top-left resize also shifts `position` (origin moves with the
 *    corner), which exercises the binding's optional-position branch in
 *    `resizeNode` — a useful side-bonus over the bottom-right gesture.
 *
 * The published-row round-trip (Y.Doc → publish → DB row) is covered by
 * the existing `yjs-publish-lifecycle` integration test plus the
 * `graph-binding` unit test for `resizeNode`. Those two together with this
 * gesture-level test cover the whole chain.
 */

async function dropFromPalette(
	page: Page,
	paletteTestId: string,
	target: Locator,
	pos: { x: number; y: number }
) {
	await page.getByTestId(paletteTestId).dragTo(target, { targetPosition: pos });
}

async function nodeBox(
	page: Page,
	testId: string
): Promise<{ x: number; y: number; width: number; height: number }> {
	const box = await page.getByTestId(testId).boundingBox();
	if (!box) throw new Error(`Node ${testId} not visible`);
	return box;
}

/**
 * Read the resized container's Y.Map entry. We scan by `type` rather than
 * id because the canvas mints the id at drop time and the test doesn't see
 * it directly. There's only one Scope (or Loop) in the test graph so the
 * scan is unambiguous.
 */
async function readResizedNodeFromYDoc(
	page: Page,
	type: 'scope' | 'loop'
): Promise<{ width: number | undefined; height: number | undefined } | null> {
	return page.evaluate((nodeType) => {
		// `__yjs_doc` is exposed by `createYjsSession` in DEV mode (see
		// `app/src/lib/yjs/session.ts`). Tests run against the dev server
		// so this is always present.
		const doc = (window as unknown as { __yjs_doc?: { getMap: (k: string) => unknown } })
			.__yjs_doc;
		if (!doc) return null;
		const nodes = doc.getMap('nodes') as { entries: () => Iterable<[string, unknown]> };
		for (const [, node] of nodes.entries()) {
			const yNode = node as { get: (k: string) => unknown };
			if (yNode.get('type') === nodeType) {
				return {
					width: yNode.get('width') as number | undefined,
					height: yNode.get('height') as number | undefined
				};
			}
		}
		return null;
	}, type);
}

test.describe('Container resize persistence', () => {
	test.beforeEach(async ({ page }) => {
		await gotoDemoEditor(page);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
		await expect(page.getByTestId('canvas-drop-zone')).toBeVisible();
	});

	test('resizing a Scope writes width/height to the Y.Doc', async ({ page }) => {
		const dropZone = page.getByTestId('canvas-drop-zone');
		const dropBounds = await dropZone.boundingBox();
		if (!dropBounds) throw new Error('drop zone not visible');

		// Drop the Scope in the center-left of the canvas. The right side of
		// the canvas is covered by the property panel once the Scope is
		// selected (selection is required for resize handles to appear), so
		// any handle that lands under the panel would be unclickable.
		const scopeDropPos = { x: dropBounds.width * 0.35, y: dropBounds.height * 0.5 };
		await dropFromPalette(page, 'palette-item-scope', dropZone, scopeDropPos);
		const scope = page.getByTestId('node-scope').first();
		await expect(scope).toBeVisible({ timeout: 5000 });

		const before = await nodeBox(page, 'node-scope');

		// Initial dims come from the canvas's drop-time default
		// (`{ width: 400, height: 200 }`). Sanity-check the seed reached
		// the Y.Doc — if it didn't, the resize assertion below would be
		// meaningless (a `null → 520` write proves less than `400 → 520`).
		const seeded = await readResizedNodeFromYDoc(page, 'scope');
		expect(seeded).not.toBeNull();
		expect(seeded!.width).toBe(400);
		expect(seeded!.height).toBe(200);

		// Select the Scope so `NodeResizer.isVisible` becomes true and the
		// resize controls render.
		await scope.click();

		const handle = page
			.locator('.svelte-flow__node-scope')
			.locator('.svelte-flow__resize-control.top.left.handle')
			.first();
		await expect(handle).toBeVisible({ timeout: 2000 });

		const handleBox = await handle.boundingBox();
		if (!handleBox) throw new Error('resize handle not visible');

		const start = {
			x: handleBox.x + handleBox.width / 2,
			y: handleBox.y + handleBox.height / 2
		};
		// Drag up-left to grow the node (top-left corner moves away from
		// the bottom-right anchor).
		const delta = { x: -150, y: -100 };

		await page.mouse.move(start.x, start.y);
		await page.mouse.down();
		await page.mouse.move(start.x + delta.x, start.y + delta.y, { steps: 8 });
		await page.mouse.up();

		// DOM assertion: xyflow applied the gesture locally.
		const after = await nodeBox(page, 'node-scope');
		expect(after.width - before.width).toBeGreaterThan(80);
		expect(after.height - before.height).toBeGreaterThan(40);

		// Persistence assertion: the resize flowed through the canvas's
		// `reportResize` context → `binding.resizeNode` → Y.Map. Without
		// the context wiring this read returns the seed dims.
		const resized = await readResizedNodeFromYDoc(page, 'scope');
		expect(resized).not.toBeNull();
		expect(resized!.width).toBeGreaterThan(seeded!.width! + 40);
		expect(resized!.height).toBeGreaterThan(seeded!.height! + 20);
	});

	test('resizing a Loop writes width/height to the Y.Doc', async ({ page }) => {
		// Same flow as Scope — different container kind, same gesture
		// chain. Catches a future regression where Loop and Scope diverge
		// (e.g. one forgets to read the resize context).
		const dropZone = page.getByTestId('canvas-drop-zone');
		const dropBounds = await dropZone.boundingBox();
		if (!dropBounds) throw new Error('drop zone not visible');

		const loopDropPos = { x: dropBounds.width * 0.35, y: dropBounds.height * 0.5 };
		await dropFromPalette(page, 'palette-item-loop', dropZone, loopDropPos);
		const loop = page.getByTestId('node-loop').first();
		await expect(loop).toBeVisible({ timeout: 5000 });

		const before = await nodeBox(page, 'node-loop');
		const seeded = await readResizedNodeFromYDoc(page, 'loop');
		expect(seeded).not.toBeNull();
		expect(seeded!.width).toBe(400);
		expect(seeded!.height).toBe(200);

		await loop.click();

		const handle = page
			.locator('.svelte-flow__node-loop')
			.locator('.svelte-flow__resize-control.top.left.handle')
			.first();
		await expect(handle).toBeVisible({ timeout: 2000 });

		const handleBox = await handle.boundingBox();
		if (!handleBox) throw new Error('resize handle not visible');

		const start = {
			x: handleBox.x + handleBox.width / 2,
			y: handleBox.y + handleBox.height / 2
		};
		// Drag up-left to grow.
		const delta = { x: -170, y: -110 };

		await page.mouse.move(start.x, start.y);
		await page.mouse.down();
		await page.mouse.move(start.x + delta.x, start.y + delta.y, { steps: 8 });
		await page.mouse.up();

		const after = await nodeBox(page, 'node-loop');
		expect(after.width - before.width).toBeGreaterThan(80);
		expect(after.height - before.height).toBeGreaterThan(40);

		const resized = await readResizedNodeFromYDoc(page, 'loop');
		expect(resized).not.toBeNull();
		expect(resized!.width).toBeGreaterThan(seeded!.width! + 40);
		expect(resized!.height).toBeGreaterThan(seeded!.height! + 20);
	});
});
