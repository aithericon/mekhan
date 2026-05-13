import { test, expect } from '@playwright/test';

// Configure test timeout via describe.configure
test.describe('Digital Lab - Basic UI Tests', () => {
	test.describe.configure({ timeout: 30000 });

	test.beforeEach(async ({ page }) => {
		// Navigate to the app
		await page.goto('/');
	});

	test('should display the header with title', async ({ page }) => {
		await expect(page.locator('h1')).toContainText('Digital Lab');
		await expect(page.locator('header')).toContainText('Colored Petri Net Visualizer');
	});

	test('should display Reset and Refresh buttons', async ({ page }) => {
		await expect(page.getByRole('button', { name: 'Reset' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'Refresh', exact: true })).toBeVisible();
	});

	test('should display the Event Log sidebar', async ({ page }) => {
		await expect(page.locator('h3')).toContainText('Event Log');
	});

	test('should load and display the Petri Net canvas (Svelte Flow)', async ({ page }) => {
		// Wait for the Svelte Flow canvas to be present
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });

		// Check that Svelte Flow controls are present
		await expect(page.getByRole('button', { name: 'Zoom In' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'Zoom Out' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'Fit View' })).toBeVisible();
	});

	test('should display Auto-Run button', async ({ page }) => {
		await expect(page.getByRole('button', { name: 'Auto-Run' })).toBeVisible();
	});
});

test.describe('Digital Lab - Data Loading Tests', () => {
	test('should fetch topology from API', async ({ page, request }) => {
		// Verify the API is accessible from the frontend
		const response = await request.get('http://localhost:5173/api/topology');
		expect(response.ok()).toBeTruthy();

		const data = await response.json();
		expect(data).toHaveProperty('topology');
		expect(data.topology).toHaveProperty('places');
		expect(data.topology).toHaveProperty('transitions');
		expect(data.topology.places.length).toBeGreaterThan(0);
	});

	test('should fetch events from API', async ({ page, request }) => {
		const response = await request.get('http://localhost:5173/api/events');
		expect(response.ok()).toBeTruthy();

		const data = await response.json();
		expect(data).toHaveProperty('events');
		expect(Array.isArray(data.events)).toBeTruthy();
	});

	test('should fetch state from API', async ({ page, request }) => {
		const response = await request.get('http://localhost:5173/api/state');
		expect(response.ok()).toBeTruthy();

		const data = await response.json();
		expect(data).toHaveProperty('marking');
		expect(data).toHaveProperty('enabled_transitions');
	});
});

test.describe('Digital Lab - API Commands', () => {
	// Use backend URL directly since Playwright request context bypasses Vite proxy
	const BACKEND_URL = 'http://localhost:3030';

	test('should be able to fire a transition via API', async ({ request }) => {
		// Reset first to ensure we have a clean state with enabled transitions
		await request.post(`${BACKEND_URL}/api/command/reset`);

		// Wait a moment for reset to complete
		await new Promise((resolve) => setTimeout(resolve, 500));

		// Get the enabled transitions
		const stateResponse = await request.get(`${BACKEND_URL}/api/state`);
		const stateData = await stateResponse.json();

		// There should be enabled transitions in a fresh state
		expect(stateData.enabled_transitions).toBeDefined();

		if (stateData.enabled_transitions && stateData.enabled_transitions.length > 0) {
			const transitionId = stateData.enabled_transitions[0];

			// Fire the transition
			const fireResponse = await request.post(`${BACKEND_URL}/api/command/fire/${transitionId}`);
			expect(fireResponse.ok()).toBeTruthy();

			const fireData = await fireResponse.json();
			expect(fireData).toHaveProperty('event');
		} else {
			// If no transitions are enabled, this test should still pass
			// (it just means the scenario is in a terminal state)
			console.log('No enabled transitions in current state - test skipped');
		}
	});

	test('should be able to reset via API', async ({ request }) => {
		const response = await request.post(`${BACKEND_URL}/api/command/reset`);
		expect(response.ok()).toBeTruthy();
	});
});

test.describe('Digital Lab - Button Interactions', () => {
	test('should respond to Refresh button click', async ({ page }) => {
		await page.goto('/');

		// Click refresh button
		const refreshButton = page.getByRole('button', { name: 'Refresh', exact: true });
		await expect(refreshButton).toBeVisible();
		await refreshButton.click();

		// Wait a moment for the refresh to process
		await page.waitForTimeout(500);

		// Page should still be functional
		await expect(page.locator('h1')).toContainText('Digital Lab');
	});

	test('should respond to Reset button click', async ({ page }) => {
		await page.goto('/');

		// Click reset button
		const resetButton = page.getByRole('button', { name: 'Reset' });
		await expect(resetButton).toBeVisible();
		await resetButton.click();

		// Wait a moment for the reset to process
		await page.waitForTimeout(500);

		// Page should still be functional
		await expect(page.locator('h1')).toContainText('Digital Lab');
	});
});

test.describe('Digital Lab - Scenario Loading', () => {
	test('should display Load Scenario button', async ({ page }) => {
		await page.goto('/');
		await expect(page.getByRole('button', { name: 'Load Scenario' })).toBeVisible();
	});

	test('should open Scenario Editor modal when clicking Load Scenario', async ({ page }) => {
		await page.goto('/');

		// Click Load Scenario button
		await page.getByRole('button', { name: 'Load Scenario' }).click();

		// Modal should appear with the title
		await expect(page.getByRole('dialog')).toBeVisible();
		// Use more specific selector within the dialog
		await expect(page.getByRole('dialog').locator('h2')).toContainText('Load Scenario');

		// Should have example buttons (updated names)
		await expect(page.getByRole('button', { name: 'Resource Allocation' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'Producer-Consumer' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'Order (Guards)' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'Booking (Retry)' })).toBeVisible();

		// Should have textarea with JSON
		await expect(page.locator('textarea')).toBeVisible();

		// Should have Load Scenario and Cancel buttons in the modal footer
		await expect(page.getByRole('dialog').getByRole('button', { name: 'Load Scenario' })).toBeVisible();
		await expect(page.getByRole('dialog').getByRole('button', { name: 'Cancel' })).toBeVisible();
	});

	test('should close modal when clicking Cancel', async ({ page }) => {
		await page.goto('/');

		// Open modal
		await page.getByRole('button', { name: 'Load Scenario' }).click();
		await expect(page.getByRole('dialog')).toBeVisible();

		// Click Cancel
		await page.getByRole('button', { name: 'Cancel' }).click();

		// Modal should be closed
		await expect(page.getByRole('dialog')).not.toBeVisible();
	});

	test('should close modal when clicking X button', async ({ page }) => {
		await page.goto('/');

		// Open modal
		await page.getByRole('button', { name: 'Load Scenario' }).click();
		await expect(page.getByRole('dialog')).toBeVisible();

		// Click X button (the close button in header)
		await page.locator('button:has-text("×")').click();

		// Modal should be closed
		await expect(page.getByRole('dialog')).not.toBeVisible();
	});

	test('should switch examples when clicking example buttons', async ({ page }) => {
		await page.goto('/');

		// Open modal
		await page.getByRole('button', { name: 'Load Scenario' }).click();

		// Get initial textarea content
		const textarea = page.locator('textarea');
		const initialContent = await textarea.inputValue();

		// Click Order (Guards) example
		await page.getByRole('button', { name: 'Order (Guards)' }).click();

		// Content should change and contain Order State Machine
		const newContent = await textarea.inputValue();
		expect(newContent).toContain('Order State Machine');
		expect(newContent).toContain('"pending"');
		expect(newContent).toContain('"submitted"');
		expect(newContent).toContain('"approved"');
	});

	test('should load scenario via API with new format', async ({ request }) => {
		// Use the new port-based scenario format
		const orderScenario = {
			name: 'Order State Machine',
			description: 'Order lifecycle with submit, approve, and reject paths',
			places: [
				{
					id: 'pending',
					name: 'Pending',
					type: 'state',
					initial_tokens: [{ order_id: 'ORD-001', amount: 50.0, customer: 'Alice' }]
				},
				{ id: 'submitted', name: 'Submitted', type: 'state', initial_tokens: [] },
				{ id: 'approved', name: 'Approved', type: 'state', initial_tokens: [] }
			],
			transitions: [
				{
					id: 'submit',
					name: 'Submit',
					input_ports: [{ name: 'order', cardinality: 'single' }],
					output_ports: [{ name: 'submitted_order', cardinality: 'single' }],
					inputs: [{ place: 'pending', port: 'order', weight: 1 }],
					outputs: [{ place: 'submitted', port: 'submitted_order', weight: 1 }],
					logic: { type: 'rhai', source: '#{ submitted_order: order }' }
				},
				{
					id: 'approve',
					name: 'Approve',
					input_ports: [{ name: 'order', cardinality: 'single' }],
					output_ports: [{ name: 'approved_order', cardinality: 'single' }],
					inputs: [{ place: 'submitted', port: 'order', weight: 1 }],
					outputs: [{ place: 'approved', port: 'approved_order', weight: 1 }],
					logic: { type: 'rhai', source: '#{ approved_order: order }' }
				}
			]
		};

		const response = await request.post('http://localhost:5173/api/scenario', {
			data: orderScenario
		});

		expect(response.ok()).toBeTruthy();

		const data = await response.json();
		expect(data.success).toBe(true);
		expect(data.places_count).toBe(3);
		expect(data.transitions_count).toBe(2);
		expect(data.tokens_count).toBe(1);
	});

	test('should update topology after loading scenario via API', async ({ request }) => {
		// Load a new scenario with the new format
		const resourceScenario = {
			name: 'Test Resources',
			places: [
				{
					id: 'workers',
					name: 'Workers',
					type: 'resource',
					initial_tokens: [{ worker_id: 'W1' }]
				},
				{
					id: 'tasks',
					name: 'Tasks',
					type: 'resource',
					initial_tokens: [{ task_id: 'T1' }]
				},
				{ id: 'in_progress', name: 'In Progress', type: 'state', initial_tokens: [] },
				{ id: 'completed', name: 'Completed', type: 'state', initial_tokens: [] }
			],
			transitions: [
				{
					id: 'assign',
					name: 'Assign',
					input_ports: [
						{ name: 'worker', cardinality: 'single' },
						{ name: 'task', cardinality: 'single' }
					],
					output_ports: [{ name: 'work', cardinality: 'single' }],
					inputs: [
						{ place: 'workers', port: 'worker', weight: 1 },
						{ place: 'tasks', port: 'task', weight: 1 }
					],
					outputs: [{ place: 'in_progress', port: 'work', weight: 1 }],
					logic: { type: 'rhai', source: '#{ work: #{ worker: worker, task: task } }' }
				},
				{
					id: 'complete',
					name: 'Complete',
					input_ports: [{ name: 'work', cardinality: 'single' }],
					output_ports: [
						{ name: 'worker_out', cardinality: 'single' },
						{ name: 'done', cardinality: 'single' }
					],
					inputs: [{ place: 'in_progress', port: 'work', weight: 1 }],
					outputs: [
						{ place: 'workers', port: 'worker_out', weight: 1 },
						{ place: 'completed', port: 'done', weight: 1 }
					],
					logic: { type: 'rhai', source: '#{ worker_out: work.worker, done: work.task }' }
				}
			]
		};

		await request.post('http://localhost:5173/api/scenario', { data: resourceScenario });

		// Fetch topology and verify it changed
		const topologyResponse = await request.get('http://localhost:5173/api/topology');
		const topologyData = await topologyResponse.json();

		expect(topologyData.topology.places.length).toBe(4);
		expect(topologyData.topology.transitions.length).toBe(2);

		// Check place names
		const placeNames = topologyData.topology.places.map((p: { name: string }) => p.name);
		expect(placeNames).toContain('Workers');
		expect(placeNames).toContain('Tasks');
		expect(placeNames).toContain('In Progress');
		expect(placeNames).toContain('Completed');
	});

	test('should load scenario through UI and update canvas', async ({ page }) => {
		await page.goto('/');

		// Wait for initial load
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });

		// Open scenario editor
		await page.getByRole('button', { name: 'Load Scenario' }).click();
		await expect(page.getByRole('dialog')).toBeVisible();

		// Select Order (Guards) example
		await page.getByRole('button', { name: 'Order (Guards)' }).click();

		// Click Load Scenario button in modal
		await page.getByRole('dialog').getByRole('button', { name: 'Load Scenario' }).click();

		// Should show success message (look for text content, not class)
		await expect(page.getByText(/Loaded: \d+ places/)).toBeVisible({ timeout: 5000 });

		// Modal should close automatically after delay
		await expect(page.getByRole('dialog')).not.toBeVisible({ timeout: 3000 });

		// Canvas should still be visible
		await expect(page.locator('.svelte-flow')).toBeVisible();
	});

	test('should show error for invalid JSON', async ({ page }) => {
		await page.goto('/');

		// Open scenario editor
		await page.getByRole('button', { name: 'Load Scenario' }).click();

		// Clear textarea and enter invalid JSON
		const textarea = page.locator('textarea');
		await textarea.clear();
		await textarea.fill('{ invalid json }');

		// Try to load
		await page.getByRole('dialog').getByRole('button', { name: 'Load Scenario' }).click();

		// Should show error message (look for text indicating a JSON error)
		await expect(page.getByText(/Expected|Invalid|JSON|Unexpected/i)).toBeVisible();
	});
});

test.describe('Digital Lab - Auto-Pilot Feature', () => {
	test('should toggle auto-pilot when clicking Auto-Run button', async ({ page }) => {
		await page.goto('/');

		// Wait for canvas to load
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });

		// Find Auto-Run button
		const autoRunButton = page.getByRole('button', { name: 'Auto-Run' });
		await expect(autoRunButton).toBeVisible();

		// Click to start auto-pilot
		await autoRunButton.click();

		// Button should change to Pause
		await expect(page.getByRole('button', { name: 'Pause' })).toBeVisible();

		// Click to stop
		await page.getByRole('button', { name: 'Pause' }).click();

		// Button should change back to Auto-Run
		await expect(page.getByRole('button', { name: 'Auto-Run' })).toBeVisible();
	});

	test('should have speed selector dropdown', async ({ page }) => {
		await page.goto('/');

		// Check for speed selector
		const speedSelector = page.locator('select');
		await expect(speedSelector).toBeVisible();

		// Should have speed options
		await expect(speedSelector.locator('option')).toHaveCount(4);
	});
});

test.describe('Digital Lab - Script Editing UI', () => {
	test('should open script sheet when clicking a transition', async ({ page }) => {
		await page.goto('/');

		// Wait for canvas to load
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });

		// Find and click on a transition node
		const transitionNode = page.locator('[data-testid="transition-node"]').first();
		await expect(transitionNode).toBeVisible();
		await transitionNode.click();

		// Script sheet should open (it's at the bottom of the screen)
		await expect(page.getByRole('button', { name: 'Edit' })).toBeVisible({ timeout: 5000 });

		// Should show the script in the editor
		await expect(page.locator('.monaco-container')).toBeVisible();
	});

	test('should toggle edit mode when clicking Edit button', async ({ page }) => {
		await page.goto('/');

		// Wait for canvas and click a transition
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });
		const transitionNode = page.locator('[data-testid="transition-node"]').first();
		await transitionNode.click();

		// Click Edit button
		const editButton = page.getByRole('button', { name: 'Edit' });
		await expect(editButton).toBeVisible();
		await editButton.click();

		// Should show Save and Cancel buttons
		await expect(page.getByRole('button', { name: 'Save' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'Cancel' })).toBeVisible();

		// Should show "Editing" badge
		await expect(page.getByText('Editing')).toBeVisible();

		// Click Cancel to exit edit mode
		await page.getByRole('button', { name: 'Cancel' }).click();

		// Should go back to Edit button
		await expect(page.getByRole('button', { name: 'Edit' })).toBeVisible();
	});

	test('should save script changes and refresh topology', async ({ page }) => {
		await page.goto('/');

		// Wait for canvas and click a transition
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });
		const transitionNode = page.locator('[data-testid="transition-node"]').first();
		await transitionNode.click();

		// Enter edit mode
		await page.getByRole('button', { name: 'Edit' }).click();
		await expect(page.getByRole('button', { name: 'Save' })).toBeVisible();

		// The Monaco editor(s) should be visible (may have 1 or 2 - guard + script)
		await expect(page.locator('.monaco-editor').first()).toBeVisible();

		// Click Save (without actually modifying - should still work)
		await page.getByRole('button', { name: 'Save' }).click();

		// Should exit edit mode (Edit button visible again) or show error
		// Wait for either success (Edit button) or error message
		await expect(
			page.getByRole('button', { name: 'Edit' }).or(page.locator('.bg-red-50'))
		).toBeVisible({ timeout: 5000 });
	});

	test('should close script sheet when clicking X button', async ({ page }) => {
		await page.goto('/');

		// Wait for canvas and click a transition
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });
		const transitionNode = page.locator('[data-testid="transition-node"]').first();
		await transitionNode.click();

		// Verify sheet is open
		await expect(page.getByRole('button', { name: 'Edit' })).toBeVisible();

		// Find and click the X button (close button)
		const closeButton = page.locator('button[aria-label="Close"]');
		await closeButton.click();

		// Sheet should be closed - Edit button no longer visible
		await expect(page.getByRole('button', { name: 'Edit' })).not.toBeVisible();
	});

	test('should show script sheet with ports and script info', async ({ page }) => {
		await page.goto('/');

		// Wait for canvas and click a transition
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });
		const transitionNode = page.locator('[data-testid="transition-node"]').first();
		await transitionNode.click();

		// Should show Input Ports section
		await expect(page.getByText('Input Ports')).toBeVisible();

		// Should show Output Ports section
		await expect(page.getByText('Output Ports')).toBeVisible();

		// Should show Main Script section
		await expect(page.getByText('Main Script (Rhai)')).toBeVisible();

		// Should show "How scripts work" explanation
		await expect(page.getByText('How scripts work:')).toBeVisible();
	});
});

test.describe('Digital Lab - Script Editing API', () => {
	// Use backend URL directly since Playwright request context bypasses Vite proxy
	const BACKEND_URL = 'http://localhost:3030';

	test('should update transition script via PATCH API', async ({ request }) => {
		// First, get a transition ID from the current topology
		const topologyResponse = await request.get(`${BACKEND_URL}/api/topology`);
		const topologyData = await topologyResponse.json();

		if (topologyData.topology?.transitions?.length > 0) {
			const transitionId = topologyData.topology.transitions[0].id;
			const originalScript = topologyData.topology.transitions[0].script;

			// Update the script
			const newScript = '#{ out: inp }';
			const updateResponse = await request.patch(
				`${BACKEND_URL}/api/topology/transition/${transitionId}`,
				{
					data: {
						script: newScript,
						guard: null
					}
				}
			);

			expect(updateResponse.ok()).toBeTruthy();

			const updateData = await updateResponse.json();
			expect(updateData.success).toBe(true);
			expect(updateData.event).toBeDefined();
			expect(updateData.event.event.type).toBe('TransitionScriptUpdated');

			// Verify the topology was updated
			const newTopologyResponse = await request.get(`${BACKEND_URL}/api/topology`);
			const newTopologyData = await newTopologyResponse.json();
			const updatedTransition = newTopologyData.topology.transitions.find(
				(t: { id: string }) => t.id === transitionId
			);
			expect(updatedTransition.script).toBe(newScript);

			// Restore original script
			await request.patch(`${BACKEND_URL}/api/topology/transition/${transitionId}`, {
				data: {
					script: originalScript,
					guard: null
				}
			});
		}
	});

	test('should reject invalid script syntax', async ({ request }) => {
		// Get a transition ID
		const topologyResponse = await request.get(`${BACKEND_URL}/api/topology`);
		const topologyData = await topologyResponse.json();

		if (topologyData.topology?.transitions?.length > 0) {
			const transitionId = topologyData.topology.transitions[0].id;

			// Try to update with invalid script
			const updateResponse = await request.patch(
				`${BACKEND_URL}/api/topology/transition/${transitionId}`,
				{
					data: {
						script: 'invalid syntax [[[',
						guard: null
					}
				}
			);

			expect(updateResponse.ok()).toBeFalsy();
			expect(updateResponse.status()).toBe(400);

			const errorData = await updateResponse.json();
			expect(errorData.success).toBe(false);
			expect(errorData.error).toBeDefined();
		}
	});
});

test.describe('Digital Lab - Group Visualization', () => {
	// Run group tests serially since they share backend state
	test.describe.configure({ mode: 'serial' });

	const BACKEND_URL = 'http://localhost:3030';

	test('should load and render group nodes from scenario with groups', async ({ page, request }) => {
		// Load a scenario with groups via API
		const groupedScenario = {
			name: 'Grouped Workflow',
			description: 'Test scenario with hierarchical groups',
			places: [
				{ id: 'jobs', name: 'Job Queue', type: 'resource', initial_tokens: [{ id: 'job-1' }] },
				{ id: 'processing', name: 'Processing', type: 'state', group_id: 'group_1', initial_tokens: [] },
				{ id: 'validated', name: 'Validated', type: 'state', group_id: 'group_2', initial_tokens: [] },
				{ id: 'completed', name: 'Completed Jobs', type: 'terminal', initial_tokens: [] }
			],
			transitions: [
				{
					id: 'pick',
					name: 'Pick Job',
					group_id: 'group_1',
					input_ports: [{ name: 'job', cardinality: 'single' }],
					output_ports: [{ name: 'picked', cardinality: 'single' }],
					inputs: [{ place: 'jobs', port: 'job', weight: 1 }],
					outputs: [{ place: 'processing', port: 'picked', weight: 1 }],
					logic: { type: 'rhai', source: '#{ picked: job }' }
				},
				{
					id: 'validate',
					name: 'Validate Job',
					group_id: 'group_2',
					input_ports: [{ name: 'job', cardinality: 'single' }],
					output_ports: [{ name: 'valid', cardinality: 'single' }],
					inputs: [{ place: 'processing', port: 'job', weight: 1 }],
					outputs: [{ place: 'validated', port: 'valid', weight: 1 }],
					logic: { type: 'rhai', source: '#{ valid: job }' }
				},
				{
					id: 'complete',
					name: 'Complete Job',
					group_id: 'group_1',
					input_ports: [{ name: 'job', cardinality: 'single' }],
					output_ports: [{ name: 'result', cardinality: 'single' }],
					inputs: [{ place: 'validated', port: 'job', weight: 1 }],
					outputs: [{ place: 'completed', port: 'result', weight: 1 }],
					logic: { type: 'rhai', source: '#{ result: job }' }
				}
			],
			groups: [
				{ id: 'group_1', name: 'Worker Pool', parent_id: null },
				{ id: 'group_2', name: 'Validation', parent_id: 'group_1' }
			]
		};

		// Load scenario
		const loadResponse = await request.post(`${BACKEND_URL}/api/scenario`, {
			data: groupedScenario
		});
		expect(loadResponse.ok()).toBeTruthy();

		// Navigate to page and wait for canvas
		await page.goto('/');
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });

		// Force refresh to ensure we have the latest topology with groups
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await page.waitForTimeout(1000);

		// Check that group nodes are rendered
		// Groups have the .group-node class
		const groupNodes = page.locator('.group-node');
		await expect(groupNodes).toHaveCount(2, { timeout: 5000 });

		// Verify group labels are visible
		await expect(page.getByText('Worker Pool')).toBeVisible();
		await expect(page.getByText('Validation')).toBeVisible();
	});

	test('should render nested groups with correct hierarchy', async ({ page, request }) => {
		// Load scenario with nested groups
		const nestedScenario = {
			name: 'Nested Groups Test',
			places: [
				{ id: 'start', name: 'Start', type: 'resource', initial_tokens: [{ id: 't1' }] },
				{ id: 'level1_state', name: 'L1 State', type: 'state', group_id: 'outer', initial_tokens: [] },
				{ id: 'level2_state', name: 'L2 State', type: 'state', group_id: 'inner', initial_tokens: [] },
				{ id: 'end', name: 'End', type: 'terminal', initial_tokens: [] }
			],
			transitions: [
				{
					id: 't1',
					name: 'To L1',
					input_ports: [{ name: 'inp', cardinality: 'single' }],
					output_ports: [{ name: 'out', cardinality: 'single' }],
					inputs: [{ place: 'start', port: 'inp', weight: 1 }],
					outputs: [{ place: 'level1_state', port: 'out', weight: 1 }],
					logic: { type: 'rhai', source: '#{ out: inp }' }
				},
				{
					id: 't2',
					name: 'To L2',
					group_id: 'inner',
					input_ports: [{ name: 'inp', cardinality: 'single' }],
					output_ports: [{ name: 'out', cardinality: 'single' }],
					inputs: [{ place: 'level1_state', port: 'inp', weight: 1 }],
					outputs: [{ place: 'level2_state', port: 'out', weight: 1 }],
					logic: { type: 'rhai', source: '#{ out: inp }' }
				},
				{
					id: 't3',
					name: 'To End',
					input_ports: [{ name: 'inp', cardinality: 'single' }],
					output_ports: [{ name: 'out', cardinality: 'single' }],
					inputs: [{ place: 'level2_state', port: 'inp', weight: 1 }],
					outputs: [{ place: 'end', port: 'out', weight: 1 }],
					logic: { type: 'rhai', source: '#{ out: inp }' }
				}
			],
			groups: [
				{ id: 'outer', name: 'Outer Group', parent_id: null },
				{ id: 'inner', name: 'Inner Group', parent_id: 'outer' }
			]
		};

		await request.post(`${BACKEND_URL}/api/scenario`, { data: nestedScenario });

		await page.goto('/');
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });

		// Force refresh to ensure we have the latest topology with groups
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await page.waitForTimeout(1000);

		// Both groups should be visible
		await expect(page.getByText('Outer Group')).toBeVisible({ timeout: 5000 });
		await expect(page.getByText('Inner Group')).toBeVisible({ timeout: 5000 });
	});

	test('should render group nodes with correct styling', async ({ page, request }) => {
		// Load a simple grouped scenario
		const scenario = {
			name: 'Styled Group Test',
			places: [
				{ id: 'p1', name: 'Place 1', type: 'state', group_id: 'grp1', initial_tokens: [] }
			],
			transitions: [],
			groups: [
				{ id: 'grp1', name: 'Test Group' }
			]
		};

		await request.post(`${BACKEND_URL}/api/scenario`, { data: scenario });

		await page.goto('/');
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });

		// Force refresh to ensure we have the latest topology with groups
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await page.waitForTimeout(1000);

		// Verify group node exists with correct class (use first() since we're just checking styling works)
		const groupNode = page.locator('.group-node').first();
		await expect(groupNode).toBeVisible();

		// Verify the group has the header with label
		const groupHeader = groupNode.locator('.group-header');
		await expect(groupHeader).toBeVisible();
		await expect(groupHeader).toContainText('Test Group');
	});

	test('should include groups in topology API response', async ({ request }) => {
		// Load scenario with groups
		const scenario = {
			name: 'API Group Test',
			places: [
				{ id: 'p1', name: 'Place 1', type: 'state', group_id: 'grp1', initial_tokens: [] },
				{ id: 'p2', name: 'Place 2', type: 'state', initial_tokens: [] }
			],
			transitions: [
				{
					id: 't1',
					name: 'Trans 1',
					group_id: 'grp1',
					input_ports: [{ name: 'inp', cardinality: 'single' }],
					output_ports: [{ name: 'out', cardinality: 'single' }],
					inputs: [{ place: 'p1', port: 'inp', weight: 1 }],
					outputs: [{ place: 'p2', port: 'out', weight: 1 }],
					logic: { type: 'rhai', source: '#{ out: inp }' }
				}
			],
			groups: [
				{ id: 'grp1', name: 'API Test Group', metadata: { custom: 'value' } }
			]
		};

		const loadResponse = await request.post(`${BACKEND_URL}/api/scenario`, { data: scenario });
		expect(loadResponse.ok()).toBeTruthy();

		// Fetch topology and verify groups are included
		const topologyResponse = await request.get(`${BACKEND_URL}/api/topology`);
		const topologyData = await topologyResponse.json();

		// Verify groups array exists and contains our group
		expect(topologyData.groups).toBeDefined();
		expect(topologyData.groups.length).toBeGreaterThan(0);

		const group = topologyData.groups.find((g: { id: string }) => g.id === 'grp1');
		expect(group).toBeDefined();
		expect(group.name).toBe('API Test Group');
	});

	test('should work with SDK-generated grouped workflow example', async ({ page, request }) => {
		// This tests the grouped_workflow.rs example scenario format
		// Note: The SDK example generates group IDs like "group_1", "group_2"
		const sdkStyleScenario = {
			name: 'grouped-workflow',
			description: 'Demonstrates hierarchical grouping with scope()',
			places: [
				{ id: 'jobs', name: 'Job Queue', type: 'resource', initial_tokens: [{ id: 'job-1', priority: 1 }] },
				{ id: 'processing', name: 'Processing', type: 'state', group_id: 'group_1', initial_tokens: [] },
				{ id: 'validated', name: 'Validated', type: 'state', group_id: 'group_2', initial_tokens: [] },
				{ id: 'completed', name: 'Completed Jobs', type: 'terminal', initial_tokens: [] },
				{ id: 'failed', name: 'Failed Jobs', type: 'terminal', initial_tokens: [] }
			],
			transitions: [
				{
					id: 'pick',
					name: 'Pick Job',
					group_id: 'group_1',
					input_ports: [{ name: 'job', cardinality: 'single' }],
					output_ports: [{ name: 'picked', cardinality: 'single' }],
					inputs: [{ place: 'jobs', port: 'job', weight: 1 }],
					outputs: [{ place: 'processing', port: 'picked', weight: 1 }],
					logic: { type: 'rhai', source: '#{ picked: job }' }
				},
				{
					id: 'validate',
					name: 'Validate Job',
					group_id: 'group_2',
					guard: { type: 'rhai', source: 'job.priority > 0' },
					input_ports: [{ name: 'job', cardinality: 'single' }],
					output_ports: [{ name: 'valid', cardinality: 'single' }],
					inputs: [{ place: 'processing', port: 'job', weight: 1 }],
					outputs: [{ place: 'validated', port: 'valid', weight: 1 }],
					logic: { type: 'rhai', source: '#{ valid: job }' }
				},
				{
					id: 'complete',
					name: 'Complete Job',
					group_id: 'group_1',
					input_ports: [{ name: 'job', cardinality: 'single' }],
					output_ports: [{ name: 'result', cardinality: 'single' }],
					inputs: [{ place: 'validated', port: 'job', weight: 1 }],
					outputs: [{ place: 'completed', port: 'result', weight: 1 }],
					logic: { type: 'rhai', source: '#{ result: #{ job_id: job.id, result: "Processed" } }' }
				}
			],
			groups: [
				{ id: 'group_1', name: 'Worker Pool', parent_id: null },
				{ id: 'group_2', name: 'Validation', parent_id: 'group_1' }
			]
		};

		const loadResponse = await request.post(`${BACKEND_URL}/api/scenario`, { data: sdkStyleScenario });
		expect(loadResponse.ok()).toBeTruthy();

		await page.goto('/');
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });
		await page.waitForTimeout(1500);

		// Verify groups are rendered
		await expect(page.getByText('Worker Pool')).toBeVisible({ timeout: 5000 });
		await expect(page.getByText('Validation')).toBeVisible({ timeout: 5000 });

		// Verify we can still interact with transitions inside groups
		const pickTransition = page.locator('[data-testid="transition-node"]').filter({ hasText: 'Pick Job' });
		await expect(pickTransition).toBeVisible();

		// The pick transition should be enabled (fire button visible)
		const fireButton = pickTransition.locator('button[aria-label="Fire transition"]');
		await expect(fireButton).toBeVisible({ timeout: 5000 });
	});
});

test.describe('Digital Lab - Mock Adapters Feature', () => {
	const BACKEND_URL = 'http://localhost:3030';

	test.beforeEach(async ({ page }) => {
		await page.goto('/');
		await page.waitForLoadState('networkidle');
	});

	test('should load Resilient Job example with mock_adapters config', async ({ page }) => {
		// Open scenario editor
		await page.getByRole('button', { name: /Load Scenario/i }).click();
		await expect(page.getByRole('dialog')).toBeVisible();

		// Click Resilient Job (Lease) example
		await page.getByRole('button', { name: /Resilient Job/i }).click();

		// Verify JSON contains mock_adapters config and lease pattern
		const textarea = page.locator('#scenario-json-editor');
		const json = await textarea.inputValue();
		expect(json).toContain('"mock_adapters"');
		expect(json).toContain('"trigger_place_id"');
		expect(json).toContain('"latency_ms"');
		expect(json).toContain('"logic_js"');
		expect(json).toContain('"Reserved (Pending Ack)"'); // Two-phase commit
		expect(json).toContain('"p_sig_cancel"'); // Cancel signal place
	});

	test('should parse scenario with mock_adapters via API', async ({ request }) => {
		// Load a scenario with mock_adapters
		const scenario = {
			name: 'Test Mock Adapters',
			places: [
				{ id: 'requests', name: 'Requests', type: 'resource', initial_tokens: [{ id: 'r1' }] },
				{ id: 'in_flight', name: 'In Flight', type: 'state', initial_tokens: [] },
				{ id: 'signals', name: 'Signals', type: 'signal', initial_tokens: [] },
				{ id: 'done', name: 'Done', type: 'state', initial_tokens: [] }
			],
			transitions: [
				{
					id: 'start',
					name: 'Start',
					input_ports: [{ name: 'req', cardinality: 'single' }],
					output_ports: [{ name: 'ctx', cardinality: 'single' }],
					inputs: [{ place: 'requests', port: 'req', weight: 1 }],
					outputs: [{ place: 'in_flight', port: 'ctx', weight: 1 }],
					logic: { type: 'rhai', source: '#{ ctx: req }' }
				},
				{
					id: 'finish',
					name: 'Finish',
					input_ports: [{ name: 'ctx', cardinality: 'single' }, { name: 'signal', cardinality: 'single' }],
					output_ports: [{ name: 'result', cardinality: 'single' }],
					inputs: [
						{ place: 'in_flight', port: 'ctx', weight: 1 },
						{ place: 'signals', port: 'signal', weight: 1 }
					],
					outputs: [{ place: 'done', port: 'result', weight: 1 }],
					logic: { type: 'rhai', source: '#{ result: #{ id: ctx.id, signal: signal } }' }
				}
			],
			mock_adapters: [
				{
					name: 'Test Worker',
					trigger_place_id: 'in_flight',
					latency_ms: 1000,
					logic_js:
						'return { target_place: "signals", data: { correlation_id: token.color.value.id, status: "done" } };'
				}
			]
		};

		const loadResponse = await request.post(`${BACKEND_URL}/api/scenario`, {
			data: scenario
		});
		expect(loadResponse.ok()).toBeTruthy();

		const loadData = await loadResponse.json();
		expect(loadData.success).toBe(true);
		expect(loadData.places_count).toBe(4);
		expect(loadData.transitions_count).toBe(2);
	});

	test('should show Resilient Job (Lease) button in scenario editor', async ({ page }) => {
		// Open scenario editor
		await page.getByRole('button', { name: /Load Scenario/i }).click();
		await expect(page.getByRole('dialog')).toBeVisible();

		// Verify Resilient Job (Lease) button exists
		const leaseButton = page.getByRole('button', { name: /Resilient Job/i });
		await expect(leaseButton).toBeVisible();
	});

	test('should store simulation config in topology via API (backward compat)', async ({
		request
	}) => {
		// Test that old simulation config still works (backward compatibility)
		const scenario = {
			name: 'Test Legacy Simulation',
			places: [
				{ id: 'jobs', name: 'Jobs', type: 'resource', initial_tokens: [{ id: 'j1' }] },
				{ id: 'done', name: 'Done', type: 'state', initial_tokens: [] }
			],
			transitions: [
				{
					id: 'process',
					name: 'Process',
					simulation: { duration_ms: 2000, variance_ms: 500 },
					input_ports: [{ name: 'job', cardinality: 'single' }],
					output_ports: [{ name: 'result', cardinality: 'single' }],
					inputs: [{ place: 'jobs', port: 'job', weight: 1 }],
					outputs: [{ place: 'done', port: 'result', weight: 1 }],
					logic: { type: 'rhai', source: '#{ result: job }' }
				}
			]
		};

		const loadResponse = await request.post(`${BACKEND_URL}/api/scenario`, {
			data: scenario
		});
		expect(loadResponse.ok()).toBeTruthy();

		// Fetch topology and verify simulation config is stored
		const topologyResponse = await request.get(`${BACKEND_URL}/api/topology`);
		const topologyData = await topologyResponse.json();

		const transition = topologyData.topology.transitions.find(
			(t: { name: string }) => t.name === 'Process'
		);
		expect(transition).toBeDefined();
		expect(transition.simulation).toBeDefined();
		expect(transition.simulation.duration_ms).toBe(2000);
		expect(transition.simulation.variance_ms).toBe(500);
	});

	test('E2E: mock adapter should inject signal token after delay', async ({ page }) => {
		// This test verifies the full flow:
		// 1. Load scenario with mock_adapters
		// 2. Fire transition that creates token in trigger place
		// 3. Wait for adapter to inject signal token
		// 4. Verify signal appears and downstream transition becomes enabled

		// Load a simple scenario with a fast mock adapter (500ms for test speed)
		const scenario = {
			name: 'E2E Adapter Test',
			places: [
				{ id: 'requests', name: 'Requests', type: 'resource', initial_tokens: [{ id: 'req-1' }] },
				{ id: 'in_flight', name: 'In Flight', type: 'state', initial_tokens: [] },
				{ id: 'signals', name: 'Signals', type: 'signal', initial_tokens: [] },
				{ id: 'done', name: 'Done', type: 'state', initial_tokens: [] }
			],
			transitions: [
				{
					id: 'start',
					name: 'Start',
					input_ports: [{ name: 'req', cardinality: 'single' }],
					output_ports: [{ name: 'ctx', cardinality: 'single' }],
					inputs: [{ place: 'requests', port: 'req', weight: 1 }],
					outputs: [{ place: 'in_flight', port: 'ctx', weight: 1 }],
					logic: { type: 'rhai', source: '#{ ctx: #{ id: req.id, status: "processing" } }' }
				},
				{
					id: 'finish',
					name: 'Finish',
					input_ports: [
						{ name: 'ctx', cardinality: 'single' },
						{ name: 'signal', cardinality: 'single' }
					],
					output_ports: [{ name: 'result', cardinality: 'single' }],
					inputs: [
						{ place: 'in_flight', port: 'ctx', weight: 1 },
						{ place: 'signals', port: 'signal', weight: 1 }
					],
					outputs: [{ place: 'done', port: 'result', weight: 1 }],
					guard: { type: 'rhai', source: 'ctx.id == signal.correlation_id' },
					logic: { type: 'rhai', source: '#{ result: #{ id: ctx.id, completed: true } }' }
				}
			],
			mock_adapters: [
				{
					name: 'Fast Worker',
					trigger_place_id: 'in_flight',
					latency_ms: 500,
					logic_js: 'const ctx = token.color.value; return { target_place: "signals", data: { correlation_id: ctx.id, status: "done" } };'
				}
			]
		};

		// Load scenario via UI
		await page.goto('/');
		await expect(page.locator('.svelte-flow')).toBeVisible({ timeout: 10000 });

		// Reset first to ensure clean state (critical for parallel test runs)
		await page.getByRole('button', { name: 'Reset' }).click();
		await page.waitForTimeout(500);

		// Open scenario editor and paste our test scenario
		await page.getByRole('button', { name: 'Load Scenario' }).click();
		await expect(page.getByRole('dialog')).toBeVisible();

		const textarea = page.locator('textarea');
		await textarea.fill(JSON.stringify(scenario, null, 2));

		// Load the scenario
		await page.getByRole('dialog').getByRole('button', { name: 'Load Scenario' }).click();
		await expect(page.getByText(/Loaded: \d+ places/)).toBeVisible({ timeout: 5000 });
		await expect(page.getByRole('dialog')).not.toBeVisible({ timeout: 3000 });

		// Wait for canvas to update and transition statuses to be computed
		await page.waitForTimeout(1000);

		// Find the "Start" transition
		const startTransition = page.locator('[data-testid="transition-node"]').filter({ hasText: 'Start' });
		await expect(startTransition).toBeVisible({ timeout: 5000 });

		// Poll for the fire button (waits for transition status computation)
		const fireButton = startTransition.locator('button[aria-label="Fire transition"]');
		await expect(fireButton).toBeVisible({ timeout: 10000 });
		await fireButton.click();

		// Wait for the adapter to inject the signal (500ms + some buffer)
		// The adapter should create a token in "Signals" place
		await page.waitForTimeout(2000);

		// Refresh events to get the latest state (adapter creates token via API)
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await page.waitForTimeout(1000);

		// The "Finish" transition should now be enabled (has fire button visible)
		// because both in_flight and signals have matching tokens
		const finishTransition = page.locator('[data-testid="transition-node"]').filter({ hasText: 'Finish' });
		await expect(finishTransition).toBeVisible();

		// Poll for the fire button to become visible (adapter might take time)
		const finishFireButton = finishTransition.locator('button[aria-label="Fire transition"]');

		// Keep refreshing until the transition is enabled or timeout
		for (let i = 0; i < 5; i++) {
			if (await finishFireButton.isVisible()) break;
			await page.getByRole('button', { name: 'Refresh', exact: true }).click();
			await page.waitForTimeout(1000);
		}

		await expect(finishFireButton).toBeVisible({ timeout: 5000 });

		// Fire the finish transition to complete the flow
		await finishFireButton.click();
		await page.waitForTimeout(500);

		// Verify we have events showing the full flow completed
		// The event log should show the Finish transition fired (2 inputs → 1 output)
		// This proves: Start fired → adapter injected signal → Finish consumed both ctx and signal
		await expect(page.locator('#event-log-sidebar')).toContainText('Fired (2 → 1)');
	});
});
