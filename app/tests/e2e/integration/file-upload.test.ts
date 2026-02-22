/**
 * File upload E2E tests.
 *
 * Tests the upload endpoint (POST /api/files/upload/{id}/{node_id})
 * and the file serving endpoint (GET /api/files/{key}).
 * Also tests the browser upload flow via the IDE file tree.
 */

import { test, expect } from '@playwright/test';
import {
	AUTHOR_ID,
	BACKEND,
	apiPost,
	servicesHealthy
} from './helpers';

test.beforeAll(async () => {
	const healthy = await servicesHealthy();
	test.skip(!healthy, 'Full stack is not running — skipping file upload tests');
});

async function createTemplate(name: string): Promise<string> {
	const res = await apiPost('/api/templates', { name, author_id: AUTHOR_ID });
	expect(res.status).toBe(201);
	const template = await res.json();
	return template.id;
}

/** Upload a synthetic PNG file directly to the backend API. */
async function uploadTestImage(
	templateId: string,
	nodeId: string,
	filename: string
): Promise<{ key: string; filename: string; content_type: string; size: number }> {
	// Minimal 1x1 red PNG (67 bytes)
	const pngBytes = new Uint8Array([
		0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
		0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
		0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xde, 0x00, 0x00, 0x00,
		0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
		0x00, 0x00, 0x02, 0x00, 0x01, 0xe2, 0x21, 0xbc, 0x33, 0x00, 0x00, 0x00,
		0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82
	]);

	const formData = new FormData();
	const blob = new Blob([pngBytes], { type: 'image/png' });
	formData.append('file', blob, filename);

	const res = await fetch(
		`${BACKEND}/api/files/upload/${templateId}/${nodeId}`,
		{ method: 'POST', body: formData }
	);
	expect(res.status).toBe(201);
	return res.json();
}

test.describe('File Upload API', () => {
	test('upload image and serve it back', async () => {
		const templateId = await createTemplate(`upload-api-${Date.now()}`);

		const result = await uploadTestImage(templateId, 'start', 'test.png');

		expect(result.key).toContain('templates/');
		expect(result.key).toContain('/blobs/start/test.png');
		expect(result.filename).toBe('test.png');
		expect(result.content_type).toBe('image/png');
		expect(result.size).toBeGreaterThan(0);

		// Serve the file back
		const fileRes = await fetch(`${BACKEND}/api/files/${result.key}`);
		expect(fileRes.status).toBe(200);
		expect(fileRes.headers.get('content-type')).toBe('image/png');
		expect(fileRes.headers.get('cache-control')).toContain('immutable');

		const body = await fileRes.arrayBuffer();
		expect(body.byteLength).toBe(result.size);
	});

	test('rejects unsupported content type', async () => {
		const templateId = await createTemplate(`upload-reject-${Date.now()}`);

		const formData = new FormData();
		const blob = new Blob(['not a real exe'], { type: 'application/x-msdownload' });
		formData.append('file', blob, 'malware.exe');

		try {
			const res = await fetch(
				`${BACKEND}/api/files/upload/${templateId}/start`,
				{ method: 'POST', body: formData }
			);
			expect(res.status).toBe(400);
			const body = await res.json();
			expect(body.error).toContain('Unsupported content type');
		} catch (e) {
			// Server may close connection early (EPIPE) when rejecting — that's acceptable
			expect(String(e)).toMatch(/EPIPE|ECONNRESET|fetch failed/);
		}
	});

	test('upload PDF succeeds', async () => {
		const templateId = await createTemplate(`upload-pdf-${Date.now()}`);

		const formData = new FormData();
		// Minimal PDF header
		const pdfContent = '%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n';
		const blob = new Blob([pdfContent], { type: 'application/pdf' });
		formData.append('file', blob, 'document.pdf');

		const res = await fetch(
			`${BACKEND}/api/files/upload/${templateId}/start`,
			{ method: 'POST', body: formData }
		);
		expect(res.status).toBe(201);
		const result = await res.json();
		expect(result.filename).toBe('document.pdf');
		expect(result.content_type).toBe('application/pdf');
	});

	test('serves 404 for nonexistent file', async () => {
		const res = await fetch(`${BACKEND}/api/files/nonexistent/key/here.png`);
		expect(res.status).toBe(404);
	});
});

test.describe('File Upload via Browser', () => {
	test('upload image via file tree and view it', async ({ page }) => {
		const templateId = await createTemplate(`upload-browser-${Date.now()}`);
		await page.goto(`/templates/${templateId}/ide`);
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });
		await expect(page.getByRole('button', { name: 'Start', exact: true })).toBeVisible({ timeout: 5_000 });

		// Upload a file via the hidden file input
		// We use page.setInputFiles to set the file on the hidden input
		const fileInput = page.locator('input[type="file"]');

		// Create a small test PNG in a temp buffer
		const pngBase64 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADklEQVQI12P4z8BQDwAEgAF/QualzQAAAABJRU5ErkJggg==';
		const pngBuffer = Buffer.from(pngBase64, 'base64');

		// Trigger the upload button to make the file input active
		const uploadBtn = page.locator('button[title="Upload image"]').first();
		await uploadBtn.click();

		// Set the file on the input
		await fileInput.setInputFiles({
			name: 'test-upload.png',
			mimeType: 'image/png',
			buffer: pngBuffer
		});

		// Wait for the upload to complete — file appears in tree, tab, and viewer header
		// Use the file tree button (exact match) to avoid strict mode violations
		await expect(
			page.getByRole('button', { name: 'test-upload.png', exact: true })
		).toBeVisible({ timeout: 10_000 });

		// Since it's an image file, the ImageViewer should render (not CodeMirror)
		await expect(page.locator('img[alt="test-upload.png"]')).toBeVisible({ timeout: 5_000 });

		// CodeMirror should NOT be visible (image files use ImageViewer)
		await expect(page.locator('.cm-editor')).not.toBeVisible();
	});

	test('uploaded image persists after reload', async ({ page }) => {
		const templateId = await createTemplate(`upload-persist-${Date.now()}`);

		await page.goto(`/templates/${templateId}/ide`);
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });
		await expect(page.getByRole('button', { name: 'Start', exact: true })).toBeVisible({ timeout: 5_000 });

		// Upload via the browser UI so Yjs gets the file entry
		const fileInput = page.locator('input[type="file"]');
		const pngBase64 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADklEQVQI12P4z8BQDwAEgAF/QualzQAAAABJRU5ErkJggg==';
		const pngBuffer = Buffer.from(pngBase64, 'base64');

		const uploadBtn = page.locator('button[title="Upload image"]').first();
		await uploadBtn.click();
		await fileInput.setInputFiles({
			name: 'reload-test.png',
			mimeType: 'image/png',
			buffer: pngBuffer
		});

		// Verify upload completed
		await expect(
			page.getByRole('button', { name: 'reload-test.png', exact: true })
		).toBeVisible({ timeout: 10_000 });

		// Wait for WS sync to persist to server
		await page.waitForTimeout(3000);

		// Reload the page
		await page.reload();
		await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });
		await expect(page.getByRole('button', { name: 'Start', exact: true })).toBeVisible({ timeout: 5_000 });

		// After reload, the URL state may auto-expand the node. If not, expand manually.
		// Check if file is already visible (auto-expanded from URL restore)
		const fileBtn = page.getByRole('button', { name: 'reload-test.png', exact: true });
		const alreadyVisible = await fileBtn.isVisible().catch(() => false);
		if (!alreadyVisible) {
			// Manually expand the Start node
			await page.getByRole('button', { name: 'Start', exact: true }).click();
		}
		await expect(fileBtn).toBeVisible({ timeout: 10_000 });
	});
});
