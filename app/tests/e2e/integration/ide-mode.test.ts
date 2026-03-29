/**
 * IDE three-column layout E2E tests.
 *
 * Tests the /templates/{id}/ide page with file tree, code editor,
 * and node config panel. Requires full stack running.
 */

import { test, expect } from '@playwright/test';
import {
	AUTHOR_ID,
	apiPost,
	apiGet,
	servicesHealthy
} from './helpers';

test.beforeAll(async () => {
	const healthy = await servicesHealthy();
	test.skip(!healthy, 'Full stack is not running — skipping IDE mode tests');
});

/** Create a fresh unpublished template and return its id. */
async function createTemplate(name: string): Promise<string> {
	const res = await apiPost('/api/templates', { name, author_id: AUTHOR_ID });
	expect(res.status).toBe(201);
	const template = await res.json();
	return template.id;
}

test.describe('IDE Mode', () => {
	test('IDE loads with three columns', async ({ page }) => {
		const templateId = await createTemplate(`ide-layout-${Date.now()}`);
		await page.goto(`/templates/${templateId}/ide`);

		// Wait for the page to load
		await expect(page.getByText('Files')).toBeVisible({ timeout: 10_000 });

		// File tree (left column) — contains the "Files" header
		await expect(page.getByText('Files')).toBeVisible();

		// Editor area (center column) — shows placeholder when no file selected
		await expect(page.getByText('Select a file from the tree to start editing')).toBeVisible();

		// Config panel (right column) — shows placeholder when no node selected
		await expect(page.getByText('Select a node to configure')).toBeVisible();
	});

	test('file tree shows graph nodes', async ({ page }) => {
		const templateId = await createTemplate(`ide-tree-${Date.now()}`);
		await page.goto(`/templates/${templateId}/ide`);
		await expect(page.getByText('Files')).toBeVisible({ timeout: 10_000 });

		// Wait for WS connection so the Y.Doc has the default Start + End nodes
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });

		// The file tree should list node labels from the graph
		// Default graph has Start and End nodes
		await expect(page.getByRole('button', { name: 'Start', exact: true })).toBeVisible({ timeout: 5_000 });
		await expect(page.getByText('End')).toBeVisible();
	});

	test('create file adds to tree', async ({ page }) => {
		const templateId = await createTemplate(`ide-create-file-${Date.now()}`);
		await page.goto(`/templates/${templateId}/ide`);
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });
		await expect(page.getByRole('button', { name: 'Start', exact: true })).toBeVisible({ timeout: 5_000 });

		// Set up the dialog mock to return a filename
		page.on('dialog', async (dialog) => {
			await dialog.accept('test_script.py');
		});

		// Click the "+" (create file) button next to the Start node
		// The Plus icon button is rendered next to each node label in FileTree
		const startNodeRow = page.locator('button:has-text("Start")').first();
		const createBtn = startNodeRow.locator('..').locator('button[title="Create file"]');
		await createBtn.click();

		// The new file should appear in the file tree
		const fileTree = page.getByTestId('file-tree');
		await expect(fileTree.getByText('test_script.py')).toBeVisible({ timeout: 5_000 });
	});

	test('file opens in editor tab', async ({ page }) => {
		const templateId = await createTemplate(`ide-open-file-${Date.now()}`);
		await page.goto(`/templates/${templateId}/ide`);
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });
		await expect(page.getByRole('button', { name: 'Start', exact: true })).toBeVisible({ timeout: 5_000 });

		// Create a file
		page.on('dialog', async (dialog) => {
			await dialog.accept('main.py');
		});
		const startNodeRow = page.locator('button:has-text("Start")').first();
		const createBtn = startNodeRow.locator('..').locator('button[title="Create file"]');
		await createBtn.click();

		// File should appear in the file tree
		const fileTree = page.getByTestId('file-tree');
		await expect(fileTree.getByText('main.py')).toBeVisible({ timeout: 5_000 });

		// The code editor (CodeMirror) should be visible — proves the file opened in a tab
		await expect(page.locator('.cm-editor')).toBeVisible({ timeout: 5_000 });
	});

	test('code editing works', async ({ page }) => {
		const templateId = await createTemplate(`ide-edit-${Date.now()}`);
		await page.goto(`/templates/${templateId}/ide`);
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });
		await expect(page.getByRole('button', { name: 'Start', exact: true })).toBeVisible({ timeout: 5_000 });

		// Create and open a file
		page.on('dialog', async (dialog) => {
			await dialog.accept('script.py');
		});
		const startNodeRow = page.locator('button:has-text("Start")').first();
		const createBtn = startNodeRow.locator('..').locator('button[title="Create file"]');
		await createBtn.click();
		await expect(page.locator('.cm-editor')).toBeVisible({ timeout: 5_000 });

		// Click into the CodeMirror editor and type
		await page.locator('.cm-content').click();
		await page.keyboard.type('print("hello")');

		// Verify the text appears in the editor DOM
		await expect(page.locator('.cm-content')).toContainText('print("hello")');
	});

	test('delete file removes from tree', async ({ page }) => {
		const templateId = await createTemplate(`ide-delete-file-${Date.now()}`);
		await page.goto(`/templates/${templateId}/ide`);
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });
		await expect(page.getByRole('button', { name: 'Start', exact: true })).toBeVisible({ timeout: 5_000 });

		// Create a file
		page.on('dialog', async (dialog) => {
			await dialog.accept('to_delete.py');
		});
		const startNodeRow = page.locator('button:has-text("Start")').first();
		const createBtn = startNodeRow.locator('..').locator('button[title="Create file"]');
		await createBtn.click();
		const fileTree = page.getByTestId('file-tree');
		await expect(fileTree.getByText('to_delete.py')).toBeVisible({ timeout: 5_000 });

		// The file tree already has the file visible (auto-expanded on create).
		// Click the delete button — the trash icon is in the same row div.
		const fileRow = fileTree.locator('.group:has-text("to_delete.py")');
		await fileRow.hover();
		await fileRow.locator('button[title="Delete file"]').click({ force: true });

		// File should be removed from the tree
		await expect(fileTree.getByText('to_delete.py')).not.toBeVisible({ timeout: 5_000 });
	});

	test('publish from IDE toolbar', async ({ page }) => {
		const templateId = await createTemplate(`ide-publish-${Date.now()}`);
		await page.goto(`/templates/${templateId}/ide`);
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });

		// Should show Draft badge initially
		await expect(page.getByText('Draft')).toBeVisible();

		// Click publish button
		const publishBtn = page.getByRole('button', { name: 'Publish' });
		await expect(publishBtn).toBeEnabled();
		await publishBtn.click();

		// Should show Published badge after publish
		await expect(page.getByText('Published')).toBeVisible({ timeout: 10_000 });

		// Verify via API
		const res = await apiGet(`/api/templates/${templateId}`);
		expect(res.status).toBe(200);
		const template = await res.json();
		expect(template.published).toBe(true);
	});

	test('code sync across contexts', async ({ browser }) => {
		const templateId = await createTemplate(`ide-sync-${Date.now()}`);
		const url = `/templates/${templateId}/ide`;

		const contextA = await browser.newContext();
		const contextB = await browser.newContext();
		const pageA = await contextA.newPage();
		const pageB = await contextB.newPage();

		await pageA.goto(url);
		await pageB.goto(url);
		await expect(pageA.getByText('Connected')).toBeVisible({ timeout: 10_000 });
		await expect(pageB.getByText('Connected')).toBeVisible({ timeout: 10_000 });
		await expect(pageA.getByRole('button', { name: 'Start', exact: true })).toBeVisible({ timeout: 5_000 });
		await expect(pageB.getByRole('button', { name: 'Start', exact: true })).toBeVisible({ timeout: 5_000 });

		// Context A creates a file via the + button next to Start node
		pageA.on('dialog', async (dialog) => {
			await dialog.accept('sync_test.py');
		});
		const createBtnA = pageA.locator('button[title="Create file"]').first();
		await createBtnA.click();
		await expect(pageA.locator('.cm-editor')).toBeVisible({ timeout: 5_000 });

		// Wait for Yjs sync to propagate file creation
		await pageB.waitForTimeout(3000);

		// Context B should see the file in the tree after expanding the Start node
		const startBtnB = pageB.getByRole('button', { name: 'Start', exact: true });
		await startBtnB.click();
		await expect(pageB.getByRole('button', { name: 'sync_test.py', exact: true })).toBeVisible({ timeout: 10_000 });

		// Open the file in context B by clicking the filename button
		await pageB.getByRole('button', { name: 'sync_test.py', exact: true }).click();
		await expect(pageB.locator('.cm-editor')).toBeVisible({ timeout: 5_000 });

		await contextA.close();
		await contextB.close();
	});

	test('file changes persist across page reload', async ({ page }) => {
		const templateId = await createTemplate(`ide-persist-${Date.now()}`);

		await page.goto(`/templates/${templateId}/ide`);
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });
		await expect(page.getByRole('button', { name: 'Start', exact: true })).toBeVisible({ timeout: 5_000 });

		// Create a file and type some content
		page.on('dialog', async (dialog) => {
			await dialog.accept('persist_test.py');
		});
		const createBtn = page.locator('button[title="Create file"]').first();
		await createBtn.click();
		await expect(page.locator('.cm-editor')).toBeVisible({ timeout: 5_000 });

		// Type code into the editor
		await page.locator('.cm-content').click();
		await page.keyboard.type('result = 123');
		await expect(page.locator('.cm-content')).toContainText('result = 123');

		// Wait for WS sync to persist updates
		await page.waitForTimeout(2000);

		// Reload the page
		await page.reload();
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });
		await expect(page.getByRole('button', { name: 'Start', exact: true })).toBeVisible({ timeout: 5_000 });

		// Expand the Start node and open the persisted file
		const fileTree = page.getByTestId('file-tree');
		await fileTree.getByRole('button', { name: 'Start', exact: true }).click();
		await expect(fileTree.getByText('persist_test.py')).toBeVisible({ timeout: 10_000 });
		await fileTree.getByText('persist_test.py').click();
		await expect(page.locator('.cm-editor')).toBeVisible({ timeout: 5_000 });

		// Verify the content was persisted across reload
		await expect(page.locator('.cm-content')).toContainText('result = 123', { timeout: 10_000 });
	});
});
