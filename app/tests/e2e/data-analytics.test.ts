import { test, expect } from '@playwright/test';

// Smoke for the /data Analytics tab: deep-link lands on the tab, every panel
// shell renders (loading / error / empty states are all fine against an
// unseeded backend — the data-testid lives on the panel card itself), and
// switching back to Entries keeps the rest of the Data surface intact.

const PANELS = [
	'analytics-summary',
	'analytics-treemap',
	'analytics-breakdown',
	'analytics-servers',
	'analytics-growth'
];

test('deep-link ?tab=analytics activates the tab and renders all panels', async ({ page }) => {
	await page.goto('/data?tab=analytics');

	const trigger = page.getByTestId('data-tab-analytics');
	await expect(trigger).toBeVisible();
	await expect(trigger).toHaveAttribute('data-state', 'active');

	for (const id of PANELS) {
		await expect(page.getByTestId(id)).toBeVisible();
	}
});

test('switching to the Entries tab still shows the entries content', async ({ page }) => {
	await page.goto('/data?tab=analytics');
	await expect(page.getByTestId('analytics-summary')).toBeVisible();

	await page.getByTestId('data-tab-entries').click();
	await expect(page.getByTestId('data-tab-entries')).toHaveAttribute('data-state', 'active');
	// EntriesTab's search box is its stable landmark.
	await expect(page.getByTestId('data-search')).toBeVisible();
	// ?tab= sync keeps the URL shareable.
	await expect(page).toHaveURL(/tab=entries/);
});
