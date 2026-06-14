import { test, expect } from '@playwright/test';
import { gotoDemoEditor } from './helpers/demo';

/**
 * Phase 4 — library-node governance UI.
 *
 *  1. Promote: a published template whose caller is Admin/Owner shows the
 *     "Promote" toolbar action; it opens the branding dialog. The /promote POST
 *     is mocked so the test stays a pure FE-wiring check (the backend path is
 *     covered live), and we assert the button flips to "Manage node" after a
 *     successful promote.
 *  2. Fork: a library palette item exposes a Fork affordance that calls the
 *     real `/api/v1/library/fork` and navigates to the new template's editor.
 */
test.describe('Library governance', () => {
	test('promote dialog: published + owner template can be promoted', async ({ page }) => {
		// Mock the controlled category vocabulary the dialog fetches.
		await page.route('**/api/v1/node-library/categories', (route) =>
			route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify(['CFD', 'FEA', 'ML'])
			})
		);
		// Mock the promote POST → returns the template now stamped as a library node.
		await page.route('**/api/v1/templates/*/promote', (route) =>
			route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					id: 'demo-template-test',
					name: 'Invoice Processing Demo',
					description: 'Showcase workflow',
					graph: {},
					version: 1,
					published: true,
					template_kind: 'library_node',
					origin: 'workspace',
					coordinate: 'acme/invoice',
					presentation: { vendor: 'Acme', category: 'CFD', icon: 'cfd', color: '#1a73e8' },
					my_effective_role: 'owner',
					author_id: '00000000-0000-0000-0000-000000000000',
					created_at: new Date().toISOString(),
					updated_at: new Date().toISOString()
				})
			})
		);

		await gotoDemoEditor(page, { published: true, my_effective_role: 'owner' });
		await expect(page.getByTestId('template-editor-page')).toBeVisible();

		const promoteBtn = page.getByTestId('btn-promote-library');
		await expect(promoteBtn).toBeVisible({ timeout: 10000 });
		await expect(promoteBtn).toContainText('Promote');

		await promoteBtn.click();
		await expect(page.getByTestId('promote-form')).toBeVisible();

		await page.getByTestId('promote-coordinate').fill('acme/invoice');
		// Category select → pick CFD.
		await page.getByTestId('promote-category').click();
		await page.getByRole('option', { name: 'CFD' }).click();

		await page.getByTestId('promote-submit').click();

		// Dialog closes and the toolbar flips to the manage affordance.
		await expect(page.getByTestId('promote-form')).toBeHidden();
		await expect(page.getByTestId('btn-promote-library')).toContainText('Manage node');
		await expect(page.getByTestId('btn-demote-library')).toBeVisible();
	});

	test('lifecycle: a library node can be deprecated from the manage dialog', async ({ page }) => {
		await page.route('**/api/v1/node-library/categories', (route) =>
			route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify(['CFD', 'Data'])
			})
		);
		// The lifecycle POST returns the family now stamped deprecated.
		await page.route('**/api/v1/templates/*/lifecycle', (route) =>
			route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					id: 'demo-template-test',
					name: 'Invoice Processing Demo',
					description: 'Showcase workflow',
					graph: {},
					version: 1,
					published: true,
					template_kind: 'library_node',
					origin: 'workspace',
					coordinate: 'acme/invoice',
					lifecycle_status: 'deprecated',
					superseded_by: 'acme/invoice-2',
					presentation: { vendor: 'Acme', category: 'Data' },
					my_effective_role: 'owner',
					author_id: '00000000-0000-0000-0000-000000000000',
					created_at: new Date().toISOString(),
					updated_at: new Date().toISOString()
				})
			})
		);

		// Open the editor already-a-library-node so the toolbar shows "Manage node".
		await gotoDemoEditor(page, {
			published: true,
			my_effective_role: 'owner',
			template_kind: 'library_node',
			origin: 'workspace',
			coordinate: 'acme/invoice',
			lifecycle_status: 'active',
			presentation: { vendor: 'Acme', category: 'Data' }
		});
		await expect(page.getByTestId('template-editor-page')).toBeVisible();

		const manageBtn = page.getByTestId('btn-promote-library');
		await expect(manageBtn).toBeVisible({ timeout: 10000 });
		await expect(manageBtn).toContainText('Manage node');
		await manageBtn.click();

		// The manage dialog exposes the lifecycle section, starting active.
		await expect(page.getByTestId('lifecycle-section')).toBeVisible();
		await expect(page.getByTestId('lifecycle-status')).toContainText('active');

		await page.getByTestId('lifecycle-successor').fill('acme/invoice-2');
		await page.getByTestId('lifecycle-deprecate').click();

		// Status badge flips to deprecated; the deprecate button is now gone.
		await expect(page.getByTestId('lifecycle-status')).toContainText('deprecated');
		await expect(page.getByTestId('lifecycle-deprecate')).toHaveCount(0);
		await expect(page.getByTestId('lifecycle-reactivate')).toBeVisible();
	});

	test('fork: a library palette item forks into a new editable template', async ({ page }) => {
		await gotoDemoEditor(page);
		await expect(page.getByTestId('canvas-drop-zone')).toBeVisible();

		// Real catalogue + fork against the dev backend (only the current
		// template GET is mocked; node-library/fork flow through the proxy).
		const forkBtn = page.getByTestId('palette-library-fork-openfoam/solid-displacement');
		await expect(forkBtn).toBeAttached({ timeout: 10000 });

		// The button is hover-revealed (opacity-0); click with force.
		await forkBtn.click({ force: true });

		// Fork creates a new template family and navigates to its editor.
		await page.waitForURL(/\/templates\/(?!demo-template-test)[0-9a-f-]{36}/, {
			timeout: 15000
		});
		await expect(page.getByTestId('template-editor-page')).toBeVisible();
	});
});
