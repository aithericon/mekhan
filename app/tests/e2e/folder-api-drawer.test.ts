import { test, expect } from '@playwright/test';

// Smoke for the per-folder OpenAPI contract UI: open the seeded Demos folder
// from the Folders manager, jump to its API tab, expand a manual trigger's
// "Try it", confirm the typed invoke form renders (incl. a file input), and
// that Fire returns a 202.
const DEFAULT_WS = '00000000-0000-0000-0000-000000000000';

test.describe('Folder API drawer', () => {
	test('opens API tab, renders typed invoke form, fires a trigger', async ({ page }) => {
		// Resolve the seeded "demos" folder id via the folders API, then drive the
		// detail route directly (the manager tree selects in-place; the Try-it
		// playground lives on the folder detail API tab).
		const folders = await page.request
			.get(`/api/v1/workspaces/${DEFAULT_WS}/folders`)
			.then((r) => r.json());
		const demos = (folders as Array<{ id: string; slug: string }>).find((f) => f.slug === 'demos');
		expect(demos, 'seeded demos folder').toBeTruthy();

		await page.goto(`/folders/${demos!.id}/api`, { waitUntil: 'networkidle' });

		// The invoice trigger card → expand "Try it".
		await expect(page.getByText('/api/v1/triggers/trigger-invoice-api/invoke')).toBeVisible({
			timeout: 15000
		});
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
