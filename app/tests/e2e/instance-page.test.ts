import { test, expect } from '@playwright/test';

/**
 * Mock data for instance page tests.
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
	created_by: '00000000-0000-0000-0000-000000000001',
	created_at: '2025-01-15T10:00:00Z',
	started_at: '2025-01-15T10:00:01Z',
	completed_at: '2025-01-15T10:01:00Z',
	current_step: null,
	metadata: {}
};

const mockStateWithEngine = {
	instance_id: INSTANCE_ID,
	net_id: NET_ID,
	status: 'completed',
	events: [
		{
			sequence: 0,
			timestamp: '2025-01-15T10:00:01Z',
			event: { type: 'NetCreated', net_id: NET_ID },
			hash: 'abc123',
			previous_hash: null
		},
		{
			sequence: 1,
			timestamp: '2025-01-15T10:00:02Z',
			event: {
				type: 'TokenCreated',
				token: {
					id: 'tok-1',
					color: { type: 'Data', value: { task: 'test' } },
					created_at: '2025-01-15T10:00:02Z'
				},
				place_id: 'start/seeded',
				place_name: 'Start'
			},
			hash: 'def456',
			previous_hash: 'abc123'
		},
		{
			sequence: 2,
			timestamp: '2025-01-15T10:00:05Z',
			event: {
				type: 'TransitionFired',
				transition_id: 't1',
				transition_name: 'prepare',
				consumed_tokens: [['start/seeded', 'tok-1']],
				produced_tokens: [
					[
						'terminal',
						{
							id: 'tok-2',
							color: { type: 'Unit' },
							created_at: '2025-01-15T10:00:05Z'
						}
					]
				]
			},
			hash: 'ghi789',
			previous_hash: 'def456'
		}
	],
	event_count: 3,
	marking: {
		tokens: {
			terminal: [
				{
					id: 'tok-2',
					color: { type: 'Unit' },
					created_at: '2025-01-15T10:00:05Z'
				}
			]
		}
	},
	engine: { available: true, run_mode: 'running' },
	enabled_transitions: [],
	current_step: null
};

const mockStateEngineOffline = {
	...mockStateWithEngine,
	engine: { available: false, run_mode: null },
	enabled_transitions: []
};

const mockStateNoEngine = {
	instance_id: INSTANCE_ID,
	net_id: NET_ID,
	status: 'completed',
	events: mockStateWithEngine.events,
	event_count: 3,
	marking: mockStateWithEngine.marking,
	// No engine field at all — simulates old backend
	enabled_transitions: [],
	current_step: null
};

function setupRoutes(
	page: import('@playwright/test').Page,
	stateResponse: unknown
) {
	return Promise.all([
		page.route(`**/api/v1/instances/${INSTANCE_ID}`, (route) => {
			if (route.request().method() === 'GET') {
				route.fulfill({ json: mockInstance });
			} else {
				route.continue();
			}
		}),
		page.route(`**/api/v1/instances/${INSTANCE_ID}/state`, (route) => {
			route.fulfill({ json: stateResponse });
		})
	]);
}

test.describe('Instance Page', () => {
	test('renders instance details and marking with engine hot', async ({ page }) => {
		await setupRoutes(page, mockStateWithEngine);
		await page.goto(`/instances/${INSTANCE_ID}`);

		// Heading and status badge
		await expect(page.getByTestId('instance-heading')).toBeVisible();
		await expect(page.getByText('completed', { exact: true })).toBeVisible();

		// Engine status badge
		await expect(page.getByTestId('engine-status')).toHaveText('Engine hot');

		// Marking section visible with tokens
		await expect(page.getByTestId('marking-section')).toBeVisible();
		await expect(page.getByText('terminal')).toBeVisible();
		await expect(page.getByText('1 token')).toBeVisible();

		// Event log section visible
		const eventLog = page.getByTestId('event-log-section');
		await expect(eventLog).toBeVisible();
		await expect(eventLog.getByText('3 events')).toBeVisible();
	});

	test('renders engine offline badge when engine unavailable', async ({ page }) => {
		await setupRoutes(page, mockStateEngineOffline);
		await page.goto(`/instances/${INSTANCE_ID}`);

		await expect(page.getByTestId('instance-heading')).toBeVisible();
		await expect(page.getByTestId('engine-status')).toHaveText('Engine offline');

		// Marking still visible (from events)
		await expect(page.getByTestId('marking-section')).toBeVisible();
		await expect(page.getByText('terminal')).toBeVisible();
	});

	test('does not crash when engine field is missing (old backend)', async ({ page }) => {
		await setupRoutes(page, mockStateNoEngine);
		await page.goto(`/instances/${INSTANCE_ID}`);

		// Page renders without crash
		await expect(page.getByTestId('instance-heading')).toBeVisible();

		// No engine badge rendered
		await expect(page.getByTestId('engine-status')).not.toBeVisible();

		// Marking still renders
		await expect(page.getByTestId('marking-section')).toBeVisible();
		await expect(page.getByText('terminal')).toBeVisible();
	});

	test('event log expands and shows events', async ({ page }) => {
		await setupRoutes(page, mockStateWithEngine);
		await page.goto(`/instances/${INSTANCE_ID}`);

		// Event log collapsed by default
		const eventLogSection = page.getByTestId('event-log-section');
		await expect(eventLogSection).toBeVisible();
		await expect(page.getByText('NetCreated')).not.toBeVisible();

		// Click to expand
		await eventLogSection.getByRole('button').first().click();

		// Events visible
		await expect(page.getByText('NetCreated')).toBeVisible();
		await expect(page.getByText('TokenCreated')).toBeVisible();
		await expect(page.getByText('TransitionFired')).toBeVisible();
	});

	test('shows "no events" message when events purged', async ({ page }) => {
		const emptyState = {
			instance_id: INSTANCE_ID,
			net_id: NET_ID,
			status: 'completed',
			events: [],
			event_count: 0,
			marking: { tokens: {} },
			engine: { available: false, run_mode: null },
			enabled_transitions: [],
			current_step: null
		};
		await setupRoutes(page, emptyState);
		await page.goto(`/instances/${INSTANCE_ID}`);

		await expect(page.getByTestId('marking-section')).toBeVisible();
		await expect(page.getByText('event log may have been purged')).toBeVisible();
	});

	test('does not fetch state for created instances', async ({ page }) => {
		const createdInstance = { ...mockInstance, status: 'created', started_at: null, completed_at: null };
		let stateFetched = false;

		await page.route(`**/api/v1/instances/${INSTANCE_ID}`, (route) => {
			if (route.request().method() === 'GET') {
				route.fulfill({ json: createdInstance });
			} else {
				route.continue();
			}
		});
		await page.route(`**/api/v1/instances/${INSTANCE_ID}/state`, (route) => {
			stateFetched = true;
			route.fulfill({ json: {} });
		});

		await page.goto(`/instances/${INSTANCE_ID}`);
		await expect(page.getByTestId('instance-heading')).toBeVisible();
		await expect(page.getByText('created', { exact: true })).toBeVisible();

		// Marking section should not appear for created instances
		await expect(page.getByTestId('marking-section')).not.toBeVisible();
		expect(stateFetched).toBe(false);
	});
});
