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

		// Create an edge: drag from Start source handle to End target handle
		const startNode = page.getByTestId('node-start');
		const endNode = page.getByTestId('node-end');
		const sourceHandle = startNode.locator('.svelte-flow__handle.source').first();
		const targetHandle = endNode.locator('.svelte-flow__handle.target').first();

		await expect(sourceHandle).toBeVisible({ timeout: 5_000 });
		await expect(targetHandle).toBeVisible({ timeout: 5_000 });
		await sourceHandle.dragTo(targetHandle);

		// Verify edge was created
		const edgesBefore = page.locator('.svelte-flow__edge');
		await expect(edgesBefore.first()).toBeVisible({ timeout: 5_000 });
		const countBefore = await edgesBefore.count();
		expect(countBefore).toBeGreaterThanOrEqual(1);

		// Hover the delete zone at the edge midpoint to reveal the button
		const deleteBtn = page.getByRole('button', { name: 'Delete connection', exact: true });
		// The button exists in DOM (isVisible EdgeToolbar) but is opacity:0.
		// Hover its parent zone to reveal it, then click.
		await deleteBtn.hover({ force: true });
		await deleteBtn.click();

		// Verify edge count decreased
		const countAfter = await page.locator('.svelte-flow__edge').count();
		expect(countAfter).toBe(countBefore - 1);
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
		const startNode = pageA.getByTestId('node-start');
		const endNode = pageA.getByTestId('node-end');
		const sourceHandle = startNode.locator('.svelte-flow__handle.source').first();
		const targetHandle = endNode.locator('.svelte-flow__handle.target').first();

		await expect(sourceHandle).toBeVisible({ timeout: 5_000 });
		await expect(targetHandle).toBeVisible({ timeout: 5_000 });
		await sourceHandle.dragTo(targetHandle);

		// Wait for edge to sync to context B
		await expect(pageB.locator('.svelte-flow__edge').first()).toBeVisible({ timeout: 10_000 });
		const countBeforeB = await pageB.locator('.svelte-flow__edge').count();

		// Hover and click delete button in context A
		const deleteBtn = pageA.getByRole('button', { name: 'Delete connection', exact: true });
		await deleteBtn.hover({ force: true });
		await deleteBtn.click();

		// Wait for sync and verify edge removed in context B
		await pageB.waitForTimeout(2000);
		const countAfterB = await pageB.locator('.svelte-flow__edge').count();
		expect(countAfterB).toBe(countBeforeB - 1);

		await contextA.close();
		await contextB.close();
	});
});
