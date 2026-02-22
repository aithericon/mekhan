/**
 * Demo page backward compatibility tests.
 *
 * The /demo page uses local state (not Yjs) for its workflow editor.
 * These tests verify it continues to work without requiring WS connections.
 */

import { test, expect } from '@playwright/test';

test.describe('Demo Page Compatibility', () => {
	test('demo page loads without WS errors', async ({ page }) => {
		const consoleErrors: string[] = [];
		page.on('console', (msg) => {
			if (msg.type() === 'error') {
				consoleErrors.push(msg.text());
			}
		});

		await page.goto('/demo');
		await expect(page.getByTestId('demo-page')).toBeVisible();

		// Toolbar and canvas should render
		await expect(page.getByTestId('demo-toolbar')).toBeVisible();
		await expect(page.getByTestId('canvas-container')).toBeVisible();

		// No WebSocket errors should appear in the console
		const wsErrors = consoleErrors.filter(
			(e) => e.toLowerCase().includes('websocket') || e.toLowerCase().includes('ws://')
		);
		expect(wsErrors).toHaveLength(0);
	});

	test('drag-and-drop still works', async ({ page }) => {
		await page.goto('/demo');
		await expect(page.getByTestId('demo-page')).toBeVisible();

		// Count existing human task nodes before drag
		const beforeCount = await page.getByTestId('node-human-task').count();

		const dropZone = page.getByTestId('canvas-drop-zone');
		const humanTaskPalette = page.getByTestId('palette-item-human_task');

		const dropBounds = await dropZone.boundingBox();
		expect(dropBounds).toBeTruthy();

		await humanTaskPalette.dragTo(dropZone, {
			targetPosition: {
				x: dropBounds!.width / 2,
				y: dropBounds!.height / 2
			}
		});

		// After drop, there should be one more human task node
		await expect(page.getByTestId('node-human-task')).toHaveCount(beforeCount + 1, {
			timeout: 5_000
		});
	});

	test('preview AIR still works', async ({ page }) => {
		await page.goto('/demo');
		await expect(page.getByTestId('demo-page')).toBeVisible();

		// Click the Preview AIR button
		await page.getByTestId('btn-preview-air').click();

		// Should show either the AIR preview panel or an error bar
		// (depending on whether the graph is valid for compilation)
		await page.waitForTimeout(1000);

		const airPreview = page.getByTestId('air-preview-panel');
		const errorBar = page.locator('.bg-amber-50');

		const hasPreview = await airPreview.isVisible();
		const hasError = await errorBar.isVisible();

		// One of the two should be visible — compilation ran
		expect(hasPreview || hasError).toBe(true);

		if (hasPreview) {
			// AIR preview should contain JSON output
			const preContent = await airPreview.locator('pre').textContent();
			expect(preContent).toBeTruthy();
			// Should be valid JSON
			expect(() => JSON.parse(preContent!)).not.toThrow();
		}
	});
});
