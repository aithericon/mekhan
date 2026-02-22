import { test, expect } from '@playwright/test';

test.describe('Navigation', () => {
	test('home page loads and shows navigation links', async ({ page }) => {
		await page.goto('/');
		await expect(page.getByTestId('home-page')).toBeVisible();
		await expect(page.getByTestId('nav-bar')).toBeVisible();
		await expect(page.getByTestId('nav-templates')).toBeVisible();
		await expect(page.getByTestId('nav-instances')).toBeVisible();
	});

	test('navigates to templates page via nav bar', async ({ page }) => {
		await page.goto('/');
		await page.getByTestId('nav-templates').click();
		await expect(page).toHaveURL('/templates');
		await expect(page.getByTestId('templates-page')).toBeVisible();
	});

	test('navigates to instances page via nav bar', async ({ page }) => {
		await page.goto('/');
		await page.getByTestId('nav-instances').click();
		await expect(page).toHaveURL('/instances');
		await expect(page.getByTestId('instances-page')).toBeVisible();
	});

	test('navigates to templates page via home button', async ({ page }) => {
		await page.goto('/');
		await page.getByTestId('btn-view-templates').click();
		await expect(page).toHaveURL('/templates');
		await expect(page.getByTestId('templates-page')).toBeVisible();
	});

	test('navigates to instances page via home button', async ({ page }) => {
		await page.goto('/');
		await page.getByTestId('btn-view-instances').click();
		await expect(page).toHaveURL('/instances');
		await expect(page.getByTestId('instances-page')).toBeVisible();
	});

	test('can navigate between templates and instances', async ({ page }) => {
		await page.goto('/templates');
		await expect(page.getByTestId('templates-page')).toBeVisible();

		await page.getByTestId('nav-instances').click();
		await expect(page).toHaveURL('/instances');
		await expect(page.getByTestId('instances-page')).toBeVisible();

		await page.getByTestId('nav-templates').click();
		await expect(page).toHaveURL('/templates');
		await expect(page.getByTestId('templates-page')).toBeVisible();
	});

	test('can return to home via logo', async ({ page }) => {
		await page.goto('/templates');
		await page.getByTestId('nav-home').click();
		await expect(page).toHaveURL('/');
		await expect(page.getByTestId('home-page')).toBeVisible();
	});

	test('demo page loads with editor', async ({ page }) => {
		await page.goto('/demo');
		await expect(page.getByTestId('demo-page')).toBeVisible();
		await expect(page.getByTestId('editor-toolbar')).toBeVisible();
		await expect(page.getByTestId('canvas-container')).toBeVisible();
	});
});
