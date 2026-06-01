import type { Page } from '@playwright/test';
import { promises as fs } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// `app/package.json` is `"type": "module"`, so this file runs as ESM where the
// CJS `__dirname` global is undefined. Derive it from import.meta.url instead.
const __dirname = path.dirname(fileURLToPath(import.meta.url));

const DEMO_ID = 'demo-template-test';
const DEMO_NAME = 'Invoice Processing Demo';

let cachedGraph: unknown = null;

/// Load the literal showcase graph from `demos/invoice-processing/graph.json`
/// — the same fixture the runtime seeder publishes. Cached per-process so
/// multiple `gotoDemoEditor` calls in one test run only touch disk once.
async function loadShowcaseGraph(): Promise<unknown> {
	if (cachedGraph) return cachedGraph;
	// `app/tests/e2e/helpers/demo.ts` → repo root is four levels up.
	const repoRoot = path.resolve(__dirname, '../../../..');
	const graphPath = path.join(repoRoot, 'demos/invoice-processing/graph.json');
	const text = await fs.readFile(graphPath, 'utf8');
	cachedGraph = JSON.parse(text);
	return cachedGraph;
}

/**
 * Stand up the demo template editor for non-integration e2e tests.
 *
 * The /demo route was retired in favor of seeding a real template and using
 * the consolidated /templates/[id] editor. For unit-style e2e tests that
 * don't run the full backend, we mock the API responses so the editor loads
 * the showcase graph deterministically — sourced from the canonical disk
 * fixture so it stays aligned with what the service seeder publishes.
 */
export async function gotoDemoEditor(page: Page) {
	const graph = await loadShowcaseGraph();
	await page.route('**/api/v1/templates/' + DEMO_ID, async (route) => {
		if (route.request().method() === 'GET') {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({
					id: DEMO_ID,
					name: DEMO_NAME,
					description: 'Showcase workflow',
					graph,
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
