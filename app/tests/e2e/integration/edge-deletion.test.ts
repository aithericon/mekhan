/**
 * Edge deletion E2E tests.
 *
 * Tests that edges can be removed via the delete button that appears
 * on hover, and that deletion syncs across Yjs clients.
 * Requires full stack running.
 */

import { test, expect } from '@playwright/test';
import { AUTHOR_ID, apiPost, servicesHealthy } from './helpers';

test.beforeAll(async () => {
	const healthy = await servicesHealthy();
	test.skip(!healthy, 'Full stack is not running — skipping edge deletion tests');
});

async function createTemplate(name: string): Promise<string> {
	const res = await apiPost('/api/templates', { name, author_id: AUTHOR_ID });
	expect(res.status).toBe(201);
	const template = await res.json();
	return template.id;
}

test.describe('Edge deletion', () => {
	test('delete button removes edge on hover and click', async ({ page }) => {
		const templateId = await createTemplate(`edge-delete-btn-${Date.now()}`);
		await page.goto(`/templates/${templateId}`);
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });

		// Create an edge: drag from Start source handle to End target handle.
		// The Handle component is a sibling of the inner div (not a child),
		// so we scope to the xyflow node wrapper which wraps both.
		const startWrapper = page.locator('.svelte-flow__node:has([data-testid="node-start"])');
		const endWrapper = page.locator('.svelte-flow__node:has([data-testid="node-end"])');
		const sourceHandle = startWrapper.locator('.svelte-flow__handle.source').first();
		const targetHandle = endWrapper.locator('.svelte-flow__handle.target').first();

		await expect(sourceHandle).toBeVisible({ timeout: 5_000 });
		await expect(targetHandle).toBeVisible({ timeout: 5_000 });

		// Manual mouse event sequence — xyflow handles don't work with dragTo()
		const sourceBBox = await sourceHandle.boundingBox();
		const targetBBox = await targetHandle.boundingBox();
		expect(sourceBBox).toBeTruthy();
		expect(targetBBox).toBeTruthy();
		const srcX = sourceBBox!.x + sourceBBox!.width / 2;
		const srcY = sourceBBox!.y + sourceBBox!.height / 2;
		const tgtX = targetBBox!.x + targetBBox!.width / 2;
		const tgtY = targetBBox!.y + targetBBox!.height / 2;
		await page.mouse.move(srcX, srcY);
		await page.mouse.down();
		await page.mouse.move(tgtX, tgtY, { steps: 5 });
		await page.mouse.up();

		// Verify edge was created
		const edgesBefore = page.locator('.svelte-flow__edge');
		await expect(edgesBefore.first()).toBeVisible({ timeout: 5_000 });
		const countBefore = await edgesBefore.count();
		expect(countBefore).toBeGreaterThanOrEqual(1);

		// Hover the delete zone to reveal the button, then force-click
		// The button is opacity:0 inside an EdgeToolbar overlay; we must force both hover and click
		const deleteBtn = page.getByRole('button', { name: 'Delete connection', exact: true }).first();
		await deleteBtn.hover({ force: true });
		await deleteBtn.click({ force: true });

		// Verify at least one edge was removed
		// Note: duplicate Start→End edges overlap, so both delete buttons may trigger
		const countAfter = await page.locator('.svelte-flow__edge').count();
		expect(countAfter).toBeLessThan(countBefore);
	});

	test('edge deletion syncs across clients', async ({ browser }) => {
		const templateId = await createTemplate(`edge-delete-sync-${Date.now()}`);
		const url = `/templates/${templateId}`;

		const contextA = await browser.newContext();
		const contextB = await browser.newContext();
		const pageA = await contextA.newPage();
		const pageB = await contextB.newPage();

		await pageA.goto(url);
		await pageB.goto(url);
		await expect(pageA.getByText('Connected')).toBeVisible({ timeout: 10_000 });
		await expect(pageB.getByText('Connected')).toBeVisible({ timeout: 10_000 });

		// Create an edge in context A
		const startWrapper = pageA.locator('.svelte-flow__node:has([data-testid="node-start"])');
		const endWrapper = pageA.locator('.svelte-flow__node:has([data-testid="node-end"])');
		const sourceHandle = startWrapper.locator('.svelte-flow__handle.source').first();
		const targetHandle = endWrapper.locator('.svelte-flow__handle.target').first();

		await expect(sourceHandle).toBeVisible({ timeout: 5_000 });
		await expect(targetHandle).toBeVisible({ timeout: 5_000 });

		// Manual mouse event sequence — xyflow handles don't work with dragTo()
		const sourceBBox = await sourceHandle.boundingBox();
		const targetBBox = await targetHandle.boundingBox();
		expect(sourceBBox).toBeTruthy();
		expect(targetBBox).toBeTruthy();
		const srcX = sourceBBox!.x + sourceBBox!.width / 2;
		const srcY = sourceBBox!.y + sourceBBox!.height / 2;
		const tgtX = targetBBox!.x + targetBBox!.width / 2;
		const tgtY = targetBBox!.y + targetBBox!.height / 2;
		await pageA.mouse.move(srcX, srcY);
		await pageA.mouse.down();
		await pageA.mouse.move(tgtX, tgtY, { steps: 5 });
		await pageA.mouse.up();

		// Wait for edge to sync to context B
		await expect(pageB.locator('.svelte-flow__edge').first()).toBeVisible({ timeout: 10_000 });
		const countBeforeB = await pageB.locator('.svelte-flow__edge').count();

		// Hover and click delete button in context A
		const deleteBtn = pageA.getByRole('button', { name: 'Delete connection', exact: true }).first();
		await deleteBtn.hover({ force: true });
		await deleteBtn.click({ force: true });

		// Wait for deletion to sync to context B
		await expect(async () => {
			const countAfterB = await pageB.locator('.svelte-flow__edge').count();
			expect(countAfterB).toBe(countBeforeB - 1);
		}).toPass({ timeout: 10_000 });

		await contextA.close();
		await contextB.close();
	});
});
