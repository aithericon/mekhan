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

test('field reference narrows to the formats in the applied query', async ({ page }) => {
	const fields = page.getByTestId('rail-fields');

	// No format asserted → per-format groups render as collapsed chevron
	// toggles; a format's fields only appear once its toggle is opened.
	// (The registry is static server metadata, so this works unseeded.)
	await page.goto('/data');
	await fields.getByRole('button', { name: 'Fields' }).click();
	await expect(fields.getByTestId('rail-fields-format-csv')).toBeVisible();
	await expect(fields.getByTestId('rail-fields-format-parquet')).toBeVisible();
	await expect(fields.getByRole('button', { name: /^meta\.delimiter\b/ })).not.toBeVisible();
	await fields.getByTestId('rail-fields-format-csv').click();
	await expect(fields.getByRole('button', { name: /^meta\.delimiter\b/ })).toBeVisible();

	// format:csv applied → the csv group is expanded inline, other format
	// toggles are gone, replaced by the muted scoped-to hint.
	await page.goto('/data?q=' + encodeURIComponent('format:csv'));
	await fields.getByRole('button', { name: 'Fields' }).click();
	await expect(fields.getByRole('button', { name: /^meta\.delimiter\b/ })).toBeVisible();
	await expect(fields.getByTestId('rail-fields-format-csv')).not.toBeVisible();
	await expect(fields.getByTestId('rail-fields-format-parquet')).not.toBeVisible();
	await expect(fields).toContainText('scoped to format: csv');
});

test('schemas facet group is collapsed by default and fetches lazily', async ({ page }) => {
	// Lazy contract: a collapsed group never fetches — count group_by=schema
	// facet requests around the expand.
	let schemaFacetRequests = 0;
	page.on('request', (req) => {
		if (req.url().includes('/catalogue/facets') && req.url().includes('group_by=schema')) {
			schemaFacetRequests++;
		}
	});

	await page.goto('/data');
	const group = page.getByTestId('facet-group-schema');
	await expect(group).toBeVisible();
	// Collapsed: no rows container rendered, no fetch fired.
	await expect(page.getByTestId('facet-group-schema-rows')).not.toBeVisible();
	expect(schemaFacetRequests).toBe(0);

	await page.getByTestId('facet-group-schema-toggle').click();
	const rows = page.getByTestId('facet-group-schema-rows');
	await expect(rows).toBeVisible();
	// Expanding fired the (single) facet fetch; works unseeded — the rows
	// container then shows buckets or the empty-scope message.
	await expect.poll(() => schemaFacetRequests).toBeGreaterThan(0);
});

test('datatype term renders a chip and round-trips ?q=', async ({ page }) => {
	await page.goto('/data');

	const input = page.getByTestId('query-bar-input');
	await input.fill('datatype:unknown');
	await input.press('Enter');

	await expect(page).toHaveURL(/q=datatype%3Aunknown/);
	await expect(page.getByTestId('query-bar-chips')).toContainText('datatype:unknown');

	// Deep-link round-trip: the same ?q= hydrates the bar + chip again.
	await page.goto('/data?q=' + encodeURIComponent('datatype:unknown'));
	await expect(input).toHaveValue('datatype:unknown');
	await expect(page.getByTestId('query-bar-chips')).toContainText('datatype:unknown');
});

test('data-types rail section renders (empty state ok)', async ({ page }) => {
	await page.goto('/data');

	const section = page.getByTestId('rail-datatypes');
	await expect(section).toBeVisible();
	await expect(section).toContainText('Data types');
});
