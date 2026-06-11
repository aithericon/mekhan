import { test, expect } from '@playwright/test';

// Smoke for the /data Entries query interface: the ?q= deep-link hydrates the
// query bar + chips, typing a term applies it to the URL, chip removal clears
// it, and the query rail + tab switching keep the query intact. Works against
// an unseeded backend — the assertions are about the bar/URL state machine,
// not result rows.

test('deep-link ?q= hydrates the query bar and renders term chips', async ({ page }) => {
	await page.goto('/data?q=' + encodeURIComponent('format:csv'));

	const input = page.getByTestId('query-bar-input');
	await expect(input).toBeVisible();
	await expect(input).toHaveValue('format:csv');

	const chips = page.getByTestId('query-bar-chips');
	await expect(chips).toBeVisible();
	await expect(chips).toContainText('format:csv');
});

test('typing a term + Enter updates ?q= and the chips row', async ({ page }) => {
	await page.goto('/data');

	const input = page.getByTestId('query-bar-input');
	await input.fill('category:dataset');
	await input.press('Enter');

	await expect(page).toHaveURL(/q=category%3Adataset/);
	await expect(page.getByTestId('query-bar-chips')).toContainText('category:dataset');
});

test('removing a chip clears the ?q= param', async ({ page }) => {
	await page.goto('/data?q=' + encodeURIComponent('format:csv'));

	const chips = page.getByTestId('query-bar-chips');
	await expect(chips).toContainText('format:csv');
	await chips.getByTitle('Remove term').click();

	await expect(page.getByTestId('query-bar-input')).toHaveValue('');
	await expect(page).not.toHaveURL(/[?&]q=/);
});

test('query rail shows on Entries only and the query survives a tab round-trip', async ({ page }) => {
	await page.goto('/data?q=' + encodeURIComponent('format:csv'));

	// The rail with its facet groups: Format expanded by default, PII collapsed
	// (heavy dimensions are lazy — they only fetch when opened).
	const rail = page.getByTestId('data-query-rail');
	await expect(rail).toBeVisible();
	await expect(page.getByTestId('facet-group-format')).toBeVisible();
	await expect(page.getByTestId('facet-group-classification-toggle')).toBeVisible();

	await page.getByTestId('data-tab-analytics').click();
	await expect(page.getByTestId('analytics-summary')).toBeVisible();
	await expect(rail).not.toBeVisible();

	await page.getByTestId('data-tab-entries').click();
	await expect(page.getByTestId('data-tab-entries')).toHaveAttribute('data-state', 'active');
	await expect(rail).toBeVisible();
	await expect(page.getByTestId('query-bar-input')).toHaveValue('format:csv');
	await expect(page).toHaveURL(/q=format%3Acsv/);
});

test('field reference inserts a stub into the draft without applying', async ({ page }) => {
	await page.goto('/data');

	await page.getByTestId('rail-fields').getByRole('button', { name: 'Fields' }).click();
	await page.getByTestId('rail-fields').getByRole('button', { name: /^name\b/ }).click();

	await expect(page.getByTestId('query-bar-input')).toHaveValue('name:');
	// Insert is draft-only — nothing executed, so no ?q= yet.
	await expect(page).not.toHaveURL(/[?&]q=/);
});
