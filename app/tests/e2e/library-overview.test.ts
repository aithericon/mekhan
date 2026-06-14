import { test, expect } from '@playwright/test';

/**
 * /library — the node-library management overview route.
 *
 *  1. Wiring (mocked): the list renders a row per descriptor, and the
 *     Manage/Demote affordances are gated to admin+ AND origin !== 'system'
 *     (system nodes are governance-locked by the backend). Search filters.
 *  2. Live: the route loads real data from GET /api/v1/node-library and shows
 *     the seeded OpenFOAM library node — proves the listNodeLibrary client +
 *     endpoint round-trip in a browser.
 */
test.describe('Node library overview', () => {
	test('lists nodes and gates Manage by origin + role', async ({ page }) => {
		await page.route('**/api/v1/node-library*', (route) =>
			route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify([
					{
						coordinate: 'acme/mesh-prep',
						templateId: '00000000-0000-0000-0000-0000000000aa',
						version: 2,
						name: 'Acme Mesh Prep',
						description: 'Workspace-owned mesh preparation node',
						origin: 'workspace',
						lifecycleStatus: 'active',
						presentation: { vendor: 'Acme', category: 'CFD', icon: 'wind', color: '#1a73e8' },
						myEffectiveRole: 'admin'
					},
					{
						coordinate: 'openfoam/solid-displacement',
						templateId: '00000000-0000-0000-0000-00000000f001',
						version: 1,
						name: 'OpenFOAM solidDisplacementFoam',
						description: 'Platform-seeded vendor pack',
						origin: 'system',
						lifecycleStatus: 'active',
						presentation: { vendor: 'OpenFOAM', category: 'CFD', icon: 'wind', color: '#0ea5e9' },
						myEffectiveRole: 'owner'
					}
				])
			})
		);

		await page.goto('/library');
		await expect(page.getByTestId('node-library-page')).toBeVisible();

		// Both rows render.
		await expect(page.getByTestId('library-row')).toHaveCount(2);

		// Manage shows ONLY for the workspace node (admin + non-system); the
		// system node is governance-locked so it exposes no Manage/Demote.
		const manage = page.getByTestId('library-manage');
		await expect(manage).toHaveCount(1);
		const workspaceRow = page
			.getByTestId('library-row')
			.filter({ hasText: 'acme/mesh-prep' });
		await expect(workspaceRow.getByTestId('library-manage')).toBeVisible();
		const systemRow = page
			.getByTestId('library-row')
			.filter({ hasText: 'openfoam/solid-displacement' });
		await expect(systemRow.getByTestId('library-manage')).toHaveCount(0);

		// Search narrows the list.
		await page.getByTestId('library-search').fill('mesh');
		await expect(page.getByTestId('library-row')).toHaveCount(1);
		await expect(page.getByTestId('library-row')).toContainText('acme/mesh-prep');
	});

	test('loads the seeded OpenFOAM node from the live endpoint', async ({ page }) => {
		await page.goto('/library');
		await expect(page.getByTestId('node-library-page')).toBeVisible();
		// The seeded system library node round-trips through the real endpoint.
		await expect(page.getByText('openfoam/solid-displacement')).toBeVisible({ timeout: 10000 });
	});
});
