import { test, expect } from '@playwright/test';

test.describe('Template CRUD', () => {
	test('templates page shows heading and create button', async ({ page }) => {
		await page.goto('/templates');
		await expect(page.getByTestId('templates-page')).toBeVisible();
		await expect(page.getByRole('heading', { name: 'Templates' })).toBeVisible();
		await expect(page.getByTestId('btn-create-template')).toBeVisible();
	});

	test('clicking new template navigates to create flow', async ({ page }) => {
		// Mock the API to simulate a backend not being available
		// The app should fall back to navigating to /templates/new
		await page.route('**/api/v1/templates', async (route) => {
			if (route.request().method() === 'POST') {
				await route.fulfill({ status: 500, body: 'Not available' });
			} else {
				await route.fulfill({
					status: 200,
					contentType: 'application/json',
					body: JSON.stringify({ items: [], total: 0, page: 1, per_page: 20 })
				});
			}
		});

		await page.goto('/templates');
		await page.getByTestId('btn-create-template').click();

		// Should navigate to /templates/new (fallback) or /templates/<uuid>
		await expect(page).toHaveURL(/\/templates\/(new|[a-f0-9-]+)/);
	});

	test('new template form has expected fields', async ({ page }) => {
		await page.goto('/templates/new');
		await expect(page.getByTestId('new-template-page')).toBeVisible();
		await expect(page.getByTestId('new-template-form')).toBeVisible();
		await expect(page.getByTestId('input-template-name')).toBeVisible();
		await expect(page.getByTestId('input-template-description')).toBeVisible();
		await expect(page.getByTestId('btn-submit-template')).toBeVisible();
	});

	test('submit button is disabled when name is empty', async ({ page }) => {
		await page.goto('/templates/new');
		const submitBtn = page.getByTestId('btn-submit-template');
		await expect(submitBtn).toBeDisabled();

		// Type a name to enable
		await page.getByTestId('input-template-name').fill('Test Workflow');
		await expect(submitBtn).toBeEnabled();

		// Clear name to disable again
		await page.getByTestId('input-template-name').fill('');
		await expect(submitBtn).toBeDisabled();
	});

	test('template editor page loads with toolbar and canvas', async ({ page }) => {
		// Mock the template API
		await page.route('**/api/v1/templates/test-id-1', async (route) => {
			await route.fulfill({ status: 500, body: 'Not available' });
		});

		await page.goto('/templates/test-id-1');
		// Should still load editor with fallback graph
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
		await expect(page.getByTestId('editor-toolbar')).toBeVisible();
		await expect(page.getByTestId('canvas-container')).toBeVisible();
		await expect(page.getByTestId('node-palette')).toBeVisible();
	});

	test('published template editor exposes a Run button (no list round-trip)', async ({
		page
	}) => {
		await page.route('**/api/v1/templates/pub-tpl', async (route) => {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					id: 'pub-tpl',
					name: 'Shipped Flow',
					description: '',
					version: 1,
					is_latest: true,
					published: true,
					published_at: '2025-01-02T00:00:00Z',
					created_at: '2025-01-01T00:00:00Z',
					updated_at: '2025-01-02T00:00:00Z'
				})
			});
		});

		await page.goto('/templates/pub-tpl');
		await expect(page.getByTestId('editor-toolbar')).toBeVisible();

		// A published template can be started in-place: Run (primary) is
		// shown alongside New Version; Publish is gone (already published).
		await expect(page.getByTestId('btn-run-template')).toBeVisible();
		await expect(page.getByTestId('btn-new-version')).toBeVisible();
		await expect(page.getByTestId('btn-publish')).toHaveCount(0);
	});

	test('template list shows items when API returns data', async ({ page }) => {
		// Mock the templates list API
		await page.route('**/api/v1/templates?*', async (route) => {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					items: [
						{
							id: 'tpl-1',
							name: 'Test Workflow',
							description: 'A test',
							version: 1,
							is_latest: true,
							published: false,
							published_at: null,
							created_at: '2025-01-01T00:00:00Z',
							updated_at: '2025-01-01T00:00:00Z'
						},
						{
							id: 'tpl-2',
							name: 'Published Flow',
							description: 'Published',
							version: 1,
							is_latest: true,
							published: true,
							published_at: '2025-01-02T00:00:00Z',
							created_at: '2025-01-01T00:00:00Z',
							updated_at: '2025-01-02T00:00:00Z'
						}
					],
					total: 2,
					page: 1,
					per_page: 20
				})
			});
		});

		await page.goto('/templates');
		await expect(page.getByTestId('template-list')).toBeVisible();
		await expect(page.getByTestId('template-item-tpl-1')).toBeVisible();
		await expect(page.getByTestId('template-item-tpl-2')).toBeVisible();

		// Verify template names
		await expect(page.getByTestId('template-item-tpl-1')).toContainText('Test Workflow');
		await expect(page.getByTestId('template-item-tpl-2')).toContainText('Published Flow');
	});

	test('delete button is available on template items', async ({ page }) => {
		await page.route('**/api/v1/templates?*', async (route) => {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					items: [
						{
							id: 'tpl-del',
							name: 'Deletable',
							description: '',
							version: 1,
							is_latest: true,
							published: false,
							published_at: null,
							my_effective_role: 'owner',
							created_at: '2025-01-01T00:00:00Z',
							updated_at: '2025-01-01T00:00:00Z'
						}
					],
					total: 1,
					page: 1,
					per_page: 20
				})
			});
		});

		await page.goto('/templates');
		await expect(page.getByTestId('template-item-tpl-del')).toBeVisible();

		// Delete now lives in the per-row actions menu (kebab). Open it, then
		// the Delete item should be available.
		await page.getByTestId('btn-template-menu-tpl-del').click();
		const deleteBtn = page.getByTestId('btn-delete-template-tpl-del');
		await expect(deleteBtn).toBeVisible();
	});

	test('published template offers New Version which forks a draft', async ({ page }) => {
		await page.route('**/api/v1/templates?*', async (route) => {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					items: [
						{
							id: 'tpl-pub',
							name: 'Shipped Flow',
							description: '',
							version: 1,
							is_latest: true,
							published: true,
							published_at: '2025-01-02T00:00:00Z',
							my_effective_role: 'owner',
							created_at: '2025-01-01T00:00:00Z',
							updated_at: '2025-01-02T00:00:00Z'
						}
					],
					total: 1,
					page: 1,
					per_page: 20
				})
			});
		});

		// new-version forks a fresh draft with a new id; the UI should then
		// navigate into that draft's editor.
		await page.route('**/api/v1/templates/tpl-pub/new-version', async (route) => {
			expect(route.request().method()).toBe('POST');
			await route.fulfill({
				status: 201,
				contentType: 'application/json',
				body: JSON.stringify({
					id: 'tpl-pub-v2',
					name: 'Shipped Flow',
					description: '',
					version: 2,
					is_latest: true,
					published: false,
					published_at: null,
					created_at: '2025-01-03T00:00:00Z',
					updated_at: '2025-01-03T00:00:00Z'
				})
			});
		});

		await page.goto('/templates');
		await expect(page.getByTestId('template-item-tpl-pub')).toBeVisible();

		// A published row exposes "New Version" in the kebab menu (no Rename,
		// since published templates are server-locked).
		await page.getByTestId('btn-template-menu-tpl-pub').click();
		const newVersionBtn = page.getByTestId('btn-new-version-template-tpl-pub');
		await expect(newVersionBtn).toBeVisible();

		await newVersionBtn.click();
		await expect(page).toHaveURL(/\/templates\/tpl-pub-v2/);
	});
});
