import { test, expect } from '@playwright/test';

/**
 * /library/packs — the library-pack management UI.
 *
 *  1. Wiring (mocked): the list renders a card per pack from a mocked
 *     `listPacks` (GET /api/v1/library/packs), an Install dialog round-trips a
 *     pasted bundle through `importPack` (POST .../import) and refreshes, and the
 *     detail page gates Remove to a non-system pack for an admin while always
 *     offering Export. All backend calls are route-intercepted so these stay pure
 *     FE-wiring checks.
 *  2. Live (@live): the route loads real data from GET /api/v1/library/packs
 *     through the dev-server proxy and shows the seeded OpenFOAM pack — proving
 *     the `listPacks` client + endpoint round-trip in a browser.
 */

// A workspace pack the caller can manage, plus a governance-locked system pack.
const WORKSPACE_PACK = {
	id: '00000000-0000-0000-0000-0000000000a1',
	name: 'Acme CFD Pack',
	description: 'Workspace-owned CFD library nodes',
	vendor: 'Acme',
	slug: 'cfd',
	version: '2',
	origin: 'workspace',
	workspaceId: '00000000-0000-0000-0000-000000000001',
	installedAt: new Date().toISOString(),
	installedBy: '00000000-0000-0000-0000-000000000000',
	nodeCount: 3,
	myEffectiveRole: 'admin'
};

const SYSTEM_PACK = {
	id: '00000000-0000-0000-0000-0000000000f1',
	name: 'OpenFOAM Pack',
	description: 'Platform-seeded vendor pack',
	vendor: 'OpenFOAM',
	slug: 'openfoam',
	version: '1',
	origin: 'system',
	workspaceId: '00000000-0000-0000-0000-000000000001',
	installedAt: new Date().toISOString(),
	installedBy: null,
	nodeCount: 1,
	myEffectiveRole: 'owner'
};

// A library-node descriptor carried inside a pack's detail payload. The detail
// page gates Remove on whether ANY of these nodes is admin+ for the caller.
function descriptor(coordinate: string, role: string) {
	return {
		coordinate,
		templateId: `00000000-0000-0000-0000-0000000000${coordinate.length.toString().padStart(2, '0')}`,
		version: 1,
		name: coordinate,
		description: `Node ${coordinate}`,
		origin: 'workspace',
		lifecycleStatus: 'active',
		presentation: { vendor: 'Acme', category: 'CFD', icon: 'wind', color: '#1a73e8' },
		myEffectiveRole: role
	};
}

test.describe('Library packs', () => {
	test('lists pack cards from listPacks (workspace + system)', async ({ page }) => {
		await page.route('**/api/v1/library/packs', (route) => {
			// Only answer the bare list GET here (import/export/detail have their
			// own, more specific routes in the other tests).
			expect(route.request().method()).toBe('GET');
			return route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify([WORKSPACE_PACK, SYSTEM_PACK])
			});
		});

		await page.goto('/library/packs');
		await expect(page.getByTestId('library-packs-page')).toBeVisible();

		// Both packs render as cards.
		await expect(page.getByTestId('library-pack-card')).toHaveCount(2);

		const workspaceCard = page
			.getByTestId('library-pack-card')
			.filter({ hasText: 'Acme CFD Pack' });
		await expect(workspaceCard).toContainText('workspace');
		await expect(workspaceCard).toContainText('3 nodes');

		const systemCard = page.getByTestId('library-pack-card').filter({ hasText: 'OpenFOAM Pack' });
		await expect(systemCard).toContainText('system');
		await expect(systemCard).toContainText('1 node');
	});

	test('Install dialog imports a pasted bundle and refreshes the list', async ({ page }) => {
		// The list GET returns empty first, then the workspace pack after import —
		// so a successful refresh visibly adds a card.
		let imported = false;
		await page.route('**/api/v1/library/packs', (route) => {
			if (route.request().method() !== 'GET') return route.fallback();
			return route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify(imported ? [WORKSPACE_PACK] : [])
			});
		});

		// Capture the import POST body to assert its bundle shape.
		let importBody: unknown = null;
		await page.route('**/api/v1/library/packs/import', (route) => {
			expect(route.request().method()).toBe('POST');
			importBody = route.request().postDataJSON();
			imported = true;
			return route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					nodeCount: WORKSPACE_PACK.nodeCount,
					pack: WORKSPACE_PACK
				})
			});
		});

		await page.goto('/library/packs');
		await expect(page.getByTestId('library-packs-page')).toBeVisible();
		// Empty to start.
		await expect(page.getByTestId('library-pack-empty')).toBeVisible();

		// Open the install dialog and paste a minimal-but-valid pack bundle.
		await page.getByTestId('library-pack-install').click();
		await expect(page.getByTestId('library-pack-install-dialog')).toBeVisible();

		const bundle = {
			manifest: { name: 'Acme CFD Pack', vendor: 'Acme', slug: 'cfd', version: '2' },
			nodes: [
				{
					coordinate: 'acme/mesh-prep',
					name: 'Acme Mesh Prep',
					graph: {},
					presentation: { vendor: 'Acme', category: 'CFD' }
				}
			],
			assets: []
		};
		await page.getByTestId('library-pack-install-textarea').fill(JSON.stringify(bundle));
		await page.getByTestId('library-pack-install-submit').click();

		// Dialog closes and the freshly-imported pack shows up after the refresh.
		await expect(page.getByTestId('library-pack-install-dialog')).toBeHidden();
		await expect(page.getByTestId('library-pack-card')).toHaveCount(1);
		await expect(page.getByTestId('library-pack-card')).toContainText('Acme CFD Pack');

		// The POST carried the parsed bundle verbatim (manifest + nodes array).
		expect(importBody).toMatchObject({
			manifest: { name: 'Acme CFD Pack', vendor: 'Acme', slug: 'cfd' },
			nodes: [{ coordinate: 'acme/mesh-prep' }]
		});
	});

	test('detail: Remove is offered for a workspace pack and calls deletePack', async ({ page }) => {
		await page.route(`**/api/v1/library/packs/${WORKSPACE_PACK.id}`, (route) => {
			if (route.request().method() === 'GET') {
				return route.fulfill({
					status: 200,
					contentType: 'application/json',
					body: JSON.stringify({
						...WORKSPACE_PACK,
						nodes: [descriptor('acme/mesh-prep', 'admin'), descriptor('acme/solve', 'viewer')]
					})
				});
			}
			// DELETE → 204; the page then navigates back to the list.
			expect(route.request().method()).toBe('DELETE');
			return route.fulfill({ status: 204, body: '' });
		});
		// The post-delete navigation lands on the list — keep it empty.
		await page.route('**/api/v1/library/packs', (route) =>
			route.fulfill({ status: 200, contentType: 'application/json', body: '[]' })
		);
		// Auto-accept the confirm() the Remove handler raises.
		page.on('dialog', (d) => d.accept());

		await page.goto(`/library/packs/${WORKSPACE_PACK.id}`);
		await expect(page.getByTestId('library-pack-detail-page')).toBeVisible();

		// Export is always available; Remove is shown because a node is admin+.
		await expect(page.getByTestId('library-pack-export')).toBeVisible();
		const remove = page.getByTestId('library-pack-remove');
		await expect(remove).toBeVisible();

		const deleteCall = page.waitForRequest(
			(req) =>
				req.method() === 'DELETE' && req.url().includes(`/library/packs/${WORKSPACE_PACK.id}`)
		);
		await remove.click();
		await deleteCall;

		// Navigates back to the packs list after a successful remove.
		await page.waitForURL(/\/library\/packs$/);
	});

	test('detail: Remove is hidden for a system pack; Export still works', async ({ page }) => {
		await page.route(`**/api/v1/library/packs/${SYSTEM_PACK.id}`, (route) =>
			route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					...SYSTEM_PACK,
					// Even owner role can't remove a system pack — origin gates it out.
					nodes: [descriptor('openfoam/solid-displacement', 'owner')]
				})
			})
		);

		// exportPack is GET /library/packs/export?pack_id=... — intercept + assert.
		let exportCalled = false;
		await page.route('**/api/v1/library/packs/export*', (route) => {
			exportCalled = true;
			expect(route.request().url()).toContain(`pack_id=${SYSTEM_PACK.id}`);
			return route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					manifest: {
						name: SYSTEM_PACK.name,
						vendor: SYSTEM_PACK.vendor,
						slug: SYSTEM_PACK.slug,
						version: SYSTEM_PACK.version
					},
					nodes: [],
					assets: []
				})
			});
		});

		await page.goto(`/library/packs/${SYSTEM_PACK.id}`);
		await expect(page.getByTestId('library-pack-detail-page')).toBeVisible();

		// System pack is governance-locked: no Remove button at all.
		await expect(page.getByTestId('library-pack-remove')).toHaveCount(0);

		// Export still fires (the handler triggers a client-side download blob).
		const exportCall = page.waitForRequest((req) => req.url().includes('/library/packs/export'));
		await page.getByTestId('library-pack-export').click();
		await exportCall;
		expect(exportCalled).toBe(true);
	});

	test('@live loads the seeded OpenFOAM pack from the live endpoint', async ({ page }) => {
		// No route mocks: listPacks flows through the dev-server proxy to a real
		// mekhan, which seeds the OpenFOAM library pack on startup.
		await page.goto('/library/packs');
		await expect(page.getByTestId('library-packs-page')).toBeVisible();
		// At least one real pack card renders from the live list.
		await expect(page.getByTestId('library-pack-card').first()).toBeVisible({ timeout: 10000 });
	});
});
