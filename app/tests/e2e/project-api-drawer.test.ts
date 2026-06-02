import { test, expect } from '@playwright/test';

// Smoke for the per-project OpenAPI contract UI: open the API drawer from the
// seeded Demos project, expand a manual trigger's "Try it", confirm the typed
// invoke form renders (incl. a file input), and that Fire returns a 202.
const DEFAULT_WS = '00000000-0000-0000-0000-000000000000';

test.describe('Project API drawer', () => {
	test('opens drawer, renders typed invoke form, fires a trigger', async ({ page }) => {
		await page.goto(`/workspaces/${DEFAULT_WS}`, { waitUntil: 'networkidle' });

		// Demos project row is seeded; open its API drawer. Allow for SPA
		// hydration + the projects fetch before the row renders.
		await expect(page.getByTestId('project-row-demos')).toBeVisible({ timeout: 15000 });
		const apiBtn = page.getByTestId('btn-api-demos');
		await expect(apiBtn).toBeVisible({ timeout: 15000 });
		await apiBtn.click();

		// Drawer header.
		await expect(page.getByText('API · Demos')).toBeVisible();

		// The invoice trigger card → expand "Try it".
		await expect(page.getByText('/api/v1/triggers/trigger-invoice-api/invoke')).toBeVisible();
		await page.getByTestId('btn-tryit-trigger-invoice-api').click();

		// Typed form: invoice_id text + invoice_file file input.
		const invoiceId = page.locator('#f-trigger-invoice-api-invoice_id');
		const invoiceFile = page.locator('#f-trigger-invoice-api-invoice_file');
		await expect(invoiceId).toBeVisible();
		await expect(invoiceFile).toBeVisible();
		await expect(invoiceFile).toHaveAttribute('type', 'file');

		// Fire (async) with a file → expect a 202 { instance_id } rendered.
		await invoiceId.fill('E2E-UI-1');
		await invoiceFile.setInputFiles({
			name: 'invoice.png',
			mimeType: 'image/png',
			buffer: Buffer.from('\x89PNG\r\n\x1a\n', 'binary')
		});
		await page.getByRole('button', { name: /fire \(async\)/i }).click();

		await expect(page.getByText(/Accepted — still running \(202\)/)).toBeVisible({ timeout: 15000 });
		await expect(page.getByText(/"instance_id"/)).toBeVisible();
	});
});
