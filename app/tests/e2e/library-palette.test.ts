import { test, expect } from '@playwright/test';
import { gotoDemoEditor } from './helpers/demo';

/**
 * Phase 3 — the "Library" half of the editor palette.
 *
 * `gotoDemoEditor` mocks the template GET but leaves `/api/v1/node-types` and
 * `/api/v1/node-library` to flow through the dev-server proxy to a real mekhan,
 * so this exercises the genuine catalogue endpoint + the seeded system packs.
 *
 * Two proofs:
 *  1. The Library section renders, grouped, from the live catalogue.
 *  2. Dropping a library node creates a `sub_workflow` node that is branded
 *     (vendor presentation stamped) AND version-pinned (decision 12) — the
 *     payload carried through the drag and the enrichment fired in onDrop.
 */
test.describe('Library palette', () => {
	test.beforeEach(async ({ page }) => {
		await gotoDemoEditor(page);
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
		await expect(page.getByTestId('canvas-drop-zone')).toBeVisible();
	});

	test('renders the Library section from the seeded system packs', async ({ page }) => {
		await expect(page.getByTestId('palette-library-header')).toBeVisible({ timeout: 10000 });
		// Both seeded system packs surface as draggable library items.
		await expect(page.getByTestId('palette-library-openfoam/solid-displacement')).toBeVisible();
		await expect(page.getByTestId('palette-library-aithericon/hello-world')).toBeVisible();
		// Category grouping header from the controlled vocabulary (the bare "CFD"
		// text now also appears in a filter chip, so target the group header).
		await expect(page.getByTestId('palette-category-group-CFD')).toBeVisible();
	});

	test('dropping a library node creates a branded, version-pinned sub-workflow', async ({ page }) => {
		const dropZone = page.getByTestId('canvas-drop-zone');
		const dropBounds = await dropZone.boundingBox();
		if (!dropBounds) throw new Error('drop zone not visible');

		const item = page.getByTestId('palette-library-openfoam/solid-displacement');
		await expect(item).toBeVisible({ timeout: 10000 });

		// Drop in a clear region (the invoice showcase's nodes sit on the left).
		await item.dragTo(dropZone, {
			targetPosition: { x: dropBounds.width * 0.75, y: dropBounds.height * 0.3 }
		});

		const node = page.getByTestId('node-sub-workflow').last();
		await expect(node).toBeVisible({ timeout: 5000 });

		// Branded + pinned: the frozen presentation vendor and the pinned version
		// both render in the card body — proof the descriptor payload survived the
		// drag and the onDrop enrichment stamped it.
		const body = node.getByTestId('sub-workflow-body');
		await expect(body).toContainText('OpenFOAM');
		await expect(body).toContainText('v1');
	});

	test('category chips narrow the Library section to one category', async ({ page }) => {
		await expect(page.getByTestId('palette-library-header')).toBeVisible({ timeout: 10000 });

		// Chips render because more than one seeded category is present.
		await expect(page.getByTestId('palette-category-chips')).toBeVisible();
		const openfoam = page.getByTestId('palette-library-openfoam/solid-displacement');
		const helloWorld = page.getByTestId('palette-library-aithericon/hello-world');
		await expect(openfoam).toBeVisible();
		await expect(helloWorld).toBeVisible();

		// Narrow to Examples → the CFD (OpenFOAM) node drops out.
		await page.getByTestId('palette-category-chip-Examples').click();
		await expect(helloWorld).toBeVisible();
		await expect(openfoam).toHaveCount(0);

		// Back to All → both return.
		await page.getByTestId('palette-category-all').click();
		await expect(openfoam).toBeVisible();
		await expect(helloWorld).toBeVisible();
	});
});
