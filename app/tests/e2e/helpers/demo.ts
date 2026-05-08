import type { Page } from '@playwright/test';
import { showcaseGraph } from '../../../src/lib/templates/showcase';

const DEMO_ID = 'demo-template-test';
const DEMO_NAME = 'Invoice Processing Demo';

/**
 * Stand up the demo template editor for non-integration e2e tests.
 *
 * The /demo route was retired in favor of seeding a real template and using
 * the consolidated /templates/[id] editor. For unit-style e2e tests that
 * don't run the full backend, we mock the API responses so the editor loads
 * the showcase graph deterministically.
 */
export async function gotoDemoEditor(page: Page) {
	await page.route('**/api/templates/' + DEMO_ID, async (route) => {
		if (route.request().method() === 'GET') {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					id: DEMO_ID,
					name: DEMO_NAME,
					description: 'Showcase workflow',
					graph: showcaseGraph,
					version: 1,
					published: false,
					author_id: '00000000-0000-0000-0000-000000000000',
					created_at: new Date().toISOString(),
					updated_at: new Date().toISOString()
				})
			});
		} else {
			await route.continue();
		}
	});

	await page.goto(`/templates/${DEMO_ID}`);
}
