import { test, expect } from '@playwright/test';

/**
 * Instance page tests.
 *
 * The instance page was redesigned from a single scrolling page into a tabbed
 * sub-route layout (`src/routes/instances/[id]/+layout.svelte` with `process`,
 * `workflow`, `steps`, `petri-net` subroutes). The old single-page concepts
 * map onto the new UI as follows, and these tests preserve that original
 * intent against the current structure:
 *
 *   - "instance details + status"  → the always-visible header in +layout
 *     (status badge, net_id, timestamps). Driven by `getInstance`.
 *   - "marking / engine view"      → the Petri-net debug tab, which mounts the
 *     live `NetWorkbench`. The old `/state` marking section no longer exists;
 *     the engine view is now its own engine-backed tab.
 *   - "event log"                  → the Steps tab (`StepsView`), fed by the
 *     `step-executions` projection (one row per node/iteration).
 *   - "no events / purged"         → the Steps tab empty state.
 *   - "created instances do not fetch state" → a `created` instance has no net,
 *     so the layout renders the "not started yet" message, exposes no net tabs,
 *     and never fetches `/state` or `/step-executions`.
 *
 * Uses Playwright route interception so no real backend is needed.
 */

const INSTANCE_ID = '11111111-1111-1111-1111-111111111111';
const NET_ID = 'mekhan-11111111-1111-1111-1111-111111111111';
const TEMPLATE_ID = '22222222-2222-2222-2222-222222222222';

const mockInstance = {
	id: INSTANCE_ID,
	template_id: TEMPLATE_ID,
	template_version: 1,
	net_id: NET_ID,
	status: 'completed',
	mode: 'live',
	created_by: '00000000-0000-0000-0000-000000000001',
	created_at: '2025-01-15T10:00:00Z',
	started_at: '2025-01-15T10:00:01Z',
	completed_at: '2025-01-15T10:01:00Z',
	current_step: null,
	parent_instance_id: null,
	parent_node_id: null,
	result: null,
	metadata: {}
};

const mockStepExecutions = [
	{
		node_id: 'start',
		node_kind: 'Start',
		status: 'completed',
		iteration_index: 0,
		started_at: '2025-01-15T10:00:01Z',
		completed_at: '2025-01-15T10:00:02Z',
		duration_ms: 1000,
		outputs: { task: 'test' }
	},
	{
		node_id: 'prepare',
		node_kind: 'AutomatedStep',
		status: 'completed',
		iteration_index: 0,
		started_at: '2025-01-15T10:00:03Z',
		completed_at: '2025-01-15T10:00:05Z',
		duration_ms: 2000,
		outputs: { result: 'ok' }
	}
];

const mockTemplate = {
	id: TEMPLATE_ID,
	name: 'Test Template',
	version: 1,
	graph: { nodes: [], edges: [], viewport: { x: 0, y: 0, zoom: 1 } },
	interface_json: null
};

/**
 * Stub every endpoint the instance layout + its tabs touch:
 *   - GET /instances/{id}              → the instance row (header).
 *   - GET /processes?filter…           → processes spawned by the run.
 *   - GET /instances/{id}/stream       → SSE; 404 so it terminates cleanly and
 *     doesn't keep the page in a perpetual "loading/streaming" state.
 *   - GET /instances/{id}/step-executions + GET /templates/{id} → Steps tab.
 * `stepExecutions` lets each test choose the projection payload.
 */
function setupRoutes(
	page: import('@playwright/test').Page,
	opts: {
		instance?: typeof mockInstance;
		stepExecutions?: unknown[];
		onStepFetch?: () => void;
	} = {}
) {
	const instance = opts.instance ?? mockInstance;
	const steps = opts.stepExecutions ?? mockStepExecutions;
	return Promise.all([
		page.route(`**/api/v1/instances/${INSTANCE_ID}`, (route) => {
			if (route.request().method() === 'GET') {
				route.fulfill({ json: instance });
			} else {
				route.continue();
			}
		}),
		page.route('**/api/v1/processes**', (route) => {
			route.fulfill({ json: { items: [], total: 0, page: 1, page_size: 25 } });
		}),
		page.route(`**/api/v1/instances/${INSTANCE_ID}/stream`, (route) => {
			route.fulfill({ status: 404, json: { error: 'not found' } });
		}),
		page.route(`**/api/v1/instances/${INSTANCE_ID}/step-executions`, (route) => {
			opts.onStepFetch?.();
			route.fulfill({ json: steps });
		}),
		page.route(`**/api/v1/templates/${TEMPLATE_ID}`, (route) => {
			route.fulfill({ json: mockTemplate });
		})
	]);
}

test.describe('Instance Page', () => {
	test('renders instance details and exposes the petri-net (marking) tab', async ({
		page
	}) => {
		await setupRoutes(page);
		await page.goto(`/instances/${INSTANCE_ID}`);

		// Header (the always-visible summary) renders with status + net id.
		await expect(page.getByTestId('instance-page')).toBeVisible();
		await expect(page.getByText('completed', { exact: true })).toBeVisible();
		await expect(page.getByText(NET_ID)).toBeVisible();

		// A started/completed run has a net → the tab bar offers the
		// engine-backed Petri-net tab (the redesign's home for the marking view
		// that the old single page rendered inline).
		const tabs = page.getByTestId('instance-tabs');
		await expect(tabs).toBeVisible();
		const petriTab = page.getByTestId('instance-tab-petri-net');
		await expect(petriTab).toBeVisible();

		// Navigating there mounts the engine net view.
		await petriTab.click();
		await expect(page).toHaveURL(new RegExp(`/instances/${INSTANCE_ID}/petri-net$`));
		await expect(petriTab).toHaveAttribute('aria-current', 'page');
	});

	test('exposes step + workflow tabs for a started instance', async ({ page }) => {
		await setupRoutes(page);
		await page.goto(`/instances/${INSTANCE_ID}`);

		await expect(page.getByTestId('instance-page')).toBeVisible();

		// Because the run has a net, the runtime tabs (Steps / Workflow /
		// Petri-net) are present — the redesign's replacement for the old
		// always-on engine status/marking/event sections.
		await expect(page.getByTestId('instance-tab-steps')).toBeVisible();
		await expect(page.getByTestId('instance-tab-workflow')).toBeVisible();
		await expect(page.getByTestId('instance-tab-petri-net')).toBeVisible();
	});

	test('renders without crashing and shows the run header', async ({ page }) => {
		await setupRoutes(page);
		await page.goto(`/instances/${INSTANCE_ID}`);

		// Page renders without crash and the header summary is present. The
		// old test asserted the page survived a backend with no `engine` field;
		// the layout now derives everything from `getInstance` (no `/state`
		// dependency), so the equivalent robustness check is that the page
		// mounts and shows the run details.
		await expect(page.getByTestId('instance-page')).toBeVisible();
		await expect(page.getByText('completed', { exact: true })).toBeVisible();
		await expect(page.getByTestId('instance-tabs')).toBeVisible();
	});

	test('steps tab shows the step-execution rows (event log)', async ({ page }) => {
		await setupRoutes(page);
		await page.goto(`/instances/${INSTANCE_ID}/steps`);

		// The Steps tab is the redesign's event log: one row per node/iteration
		// from the step-executions projection.
		await expect(page.getByTestId('instance-tab-steps')).toHaveAttribute(
			'aria-current',
			'page'
		);
		await expect(page.getByText('start', { exact: true })).toBeVisible();
		await expect(page.getByText('prepare', { exact: true })).toBeVisible();
		await expect(page.getByText('AutomatedStep')).toBeVisible();
	});

	test('shows empty-state message when no steps have executed', async ({ page }) => {
		await setupRoutes(page, { stepExecutions: [] });
		await page.goto(`/instances/${INSTANCE_ID}/steps`);

		// Old intent: a "no events / purged" message when the projection is
		// empty. The Steps tab renders an explicit empty state instead of a
		// table.
		await expect(page.getByTestId('instance-tab-steps')).toBeVisible();
		await expect(page.getByText('No step executions yet.')).toBeVisible();
	});

	test('does not fetch step data for created (not-yet-started) instances', async ({
		page
	}) => {
		const createdInstance = {
			...mockInstance,
			status: 'created',
			started_at: null,
			completed_at: null
		};
		let stateFetched = false;
		let stepsFetched = false;

		await page.route(`**/api/v1/instances/${INSTANCE_ID}`, (route) => {
			if (route.request().method() === 'GET') {
				route.fulfill({ json: createdInstance });
			} else {
				route.continue();
			}
		});
		await page.route('**/api/v1/processes**', (route) => {
			route.fulfill({ json: { items: [], total: 0, page: 1, page_size: 25 } });
		});
		await page.route(`**/api/v1/instances/${INSTANCE_ID}/stream`, (route) => {
			route.fulfill({ status: 404, json: { error: 'not found' } });
		});
		await page.route(`**/api/v1/instances/${INSTANCE_ID}/state`, (route) => {
			stateFetched = true;
			route.fulfill({ json: {} });
		});
		await page.route(`**/api/v1/instances/${INSTANCE_ID}/step-executions`, (route) => {
			stepsFetched = true;
			route.fulfill({ json: [] });
		});

		await page.goto(`/instances/${INSTANCE_ID}`);
		await expect(page.getByTestId('instance-page')).toBeVisible();
		await expect(page.getByText('created', { exact: true })).toBeVisible();

		// A `created` instance has no net (`hasNet` is false): the layout shows
		// the "not started yet" message and exposes no runtime tabs, so no net
		// state or step projection is ever fetched.
		await expect(page.getByText('Instance has not started yet. No Petri net is available.')).toBeVisible();
		await expect(page.getByTestId('instance-tab-petri-net')).toHaveCount(0);
		await expect(page.getByTestId('instance-tab-steps')).toHaveCount(0);
		expect(stateFetched).toBe(false);
		expect(stepsFetched).toBe(false);
	});
});
