// Drive a cold browser through `/templates → click → /templates/[id]` and
// dump every phase's timing so we can stop guessing where the 10 s goes.
//
// Run with:
//   pnpm exec playwright test --config tests/cold-open-trace.config.mjs
// or directly:
//   node --experimental-vm-modules tests/cold-open-trace.mjs
//
// What it captures, per attempt:
//   - navigation start → page DOMContentLoaded → page load
//   - first `/api/yjs/<id>` WS request: requestWillBeSent → handshake 101 → first frame
//   - first `/api/templates/<id>` REST round-trip
//   - count + total wall-clock of every Vite-served `.svelte`/`.ts` request
//   - first appearance of `[data-testid="canvas-container"]` and of a node
//
// It runs the same scenario twice in one browser context: pass 1 = truly
// cold (fresh page/context), pass 2 = warm (same context, second navigation).

import { chromium } from '@playwright/test';

const BASE = process.env.BASE_URL ?? 'http://localhost:5173';

function fmt(ms) {
	return `${ms.toFixed(0).padStart(6)}ms`;
}

async function runOnce(context, label, templateId, mode = 'click') {
	const page = await context.newPage();

	const phaseTimes = {
		navStart: 0,
		domContentLoaded: 0,
		load: 0,
		firstWsRequest: 0,
		firstWsResponse: 0,
		firstWsFrame: 0,
		firstTemplateApi: 0,
		canvasContainer: 0,
		firstNode: 0
	};

	const viteRequests = [];
	let wsRequestStart = null;
	let wsResponseStart = null;
	let wsFirstFrame = null;

	page.on('request', (req) => {
		const url = req.url();
		if (url.includes('/api/yjs/')) {
			if (!wsRequestStart) wsRequestStart = Date.now();
		}
	});

	page.on('response', (res) => {
		const url = res.url();
		if (url.includes('/api/yjs/')) {
			if (!wsResponseStart) wsResponseStart = Date.now();
		}
		if (url.includes('/api/templates/') && !url.includes('/instances')) {
			if (!phaseTimes.firstTemplateApi) phaseTimes.firstTemplateApi = Date.now();
		}
		// Vite-served source files
		if (url.startsWith(BASE)) {
			const path = new URL(url).pathname;
			if (
				path.endsWith('.svelte') ||
				path.endsWith('.ts') ||
				path.endsWith('.svelte.ts') ||
				path.includes('/.vite/deps/') ||
				path.includes('/@fs/')
			) {
				viteRequests.push({ path, status: res.status() });
			}
		}
	});

	page.on('websocket', (ws) => {
		ws.on('framereceived', () => {
			if (!wsFirstFrame) wsFirstFrame = Date.now();
		});
	});

	let navStartEditor;
	let navEnd;
	if (mode === 'direct') {
		// Direct cold: hit /templates/[id] as the first URL of the context. No
		// layout/list page warming Vite first. Most realistic for a hard
		// reload on the editor URL.
		navStartEditor = Date.now();
		await page.goto(`${BASE}/templates/${templateId}`, { waitUntil: 'domcontentloaded' });
		navEnd = Date.now();
	} else {
		// Click pattern: visit /templates first (warms layout + list page
		// modules), then click a row.
		await page.goto(`${BASE}/templates`, { waitUntil: 'domcontentloaded' });
		navStartEditor = Date.now();
		const clickPromise = page.click(`[data-testid="template-item-${templateId}"]`);
		const navPromise = page.waitForURL(`**/templates/${templateId}`, { timeout: 30_000 });
		await Promise.all([clickPromise, navPromise]);
		navEnd = Date.now();
	}
	phaseTimes.navStart = navStartEditor;

	// Wait for canvas to appear
	await page
		.waitForSelector('[data-testid="canvas-container"]', { timeout: 30_000 })
		.then(() => {
			phaseTimes.canvasContainer = Date.now();
		});

	// Wait for at least one node to render — that's when WS has synced
	await page
		.waitForSelector('.svelte-flow__node', { timeout: 30_000 })
		.then(() => {
			phaseTimes.firstNode = Date.now();
		});

	phaseTimes.firstWsRequest = wsRequestStart ?? 0;
	phaseTimes.firstWsResponse = wsResponseStart ?? 0;
	phaseTimes.firstWsFrame = wsFirstFrame ?? 0;

	const offset = navStartEditor;
	console.log(`\n=== ${label} (templateId=${templateId.slice(0, 8)}) ===`);
	console.log(`  click to URL change:    ${fmt(navEnd - navStartEditor)}`);
	console.log(
		`  click → first WS req:   ${fmt((phaseTimes.firstWsRequest || navEnd) - offset)}`
	);
	console.log(
		`  click → WS 101 resp:    ${fmt((phaseTimes.firstWsResponse || navEnd) - offset)}`
	);
	console.log(
		`  click → first WS frame: ${fmt((phaseTimes.firstWsFrame || navEnd) - offset)}`
	);
	console.log(
		`  click → /api/templates: ${fmt((phaseTimes.firstTemplateApi || navEnd) - offset)}`
	);
	console.log(`  click → canvas div:     ${fmt(phaseTimes.canvasContainer - offset)}`);
	console.log(`  click → first node:     ${fmt(phaseTimes.firstNode - offset)}`);
	console.log(`  vite-served requests:   ${viteRequests.length}`);

	await page.close();
}

async function main() {
	const browser = await chromium.launch({ headless: true });
	// Fresh context = cold cache (no service worker, no cached modules)
	const context = await browser.newContext();

	// First, list templates to pick an id with content
	const probePage = await context.newPage();
	await probePage.goto(`${BASE}/templates`, { waitUntil: 'networkidle' });
	await probePage
		.waitForSelector('[data-testid^="template-item-"]', { timeout: 10_000 })
		.catch(() => {});
	const ids = await probePage.$$eval('[data-testid^="template-item-"]', (els) =>
		els.map((e) => e.getAttribute('data-testid').replace('template-item-', ''))
	);
	await probePage.close();
	if (ids.length === 0) {
		console.error('No templates found at /templates');
		process.exit(1);
	}
	const targetId = ids[0];
	console.log(`Found ${ids.length} templates; using ${targetId.slice(0, 8)}`);

	// Fresh context for the cold pass
	await context.close();

	// Pass A: brand-new context, direct URL load — most realistic for "hard
	// reload on the editor URL" which is what the user described.
	{
		const ctx = await browser.newContext();
		await runOnce(ctx, 'COLD DIRECT-URL (truly cold context)', targetId, 'direct');
		await ctx.close();
	}

	// Pass B: same context, click-through pattern (simulates list → editor)
	{
		const ctx = await browser.newContext();
		await runOnce(ctx, 'COLD CLICK-THROUGH', targetId, 'click');
		await runOnce(ctx, 'WARM CLICK-THROUGH (2nd open)', targetId, 'click');
		await ctx.close();
	}

	// Pass C: headed browser (so we see what the user sees) — controlled by env
	if (process.env.HEADED === '1') {
		const headedBrowser = await chromium.launch({ headless: false, slowMo: 0 });
		const ctx = await headedBrowser.newContext();
		await runOnce(ctx, 'COLD HEADED DIRECT-URL', targetId, 'direct');
		await ctx.close();
		await headedBrowser.close();
	}

	await browser.close();
}

main().catch((e) => {
	console.error(e);
	process.exit(1);
});
