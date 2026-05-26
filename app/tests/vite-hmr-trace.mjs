// Open the editor in a fresh context and capture every console line + every
// WebSocket lifecycle event, so we can see Vite's own HMR diagnostics.

import { chromium } from '@playwright/test';

const BASE = process.env.BASE_URL ?? 'http://localhost:5173';
const TID = process.env.TID ?? '00000000-0000-0000-0000-000000000016';

const browser = await chromium.launch({ headless: true });
const context = await browser.newContext();
const page = await context.newPage();

const start = Date.now();
const log = (msg) => console.log(`+${(Date.now() - start).toString().padStart(5)}ms ${msg}`);

page.on('console', (msg) => log(`[console:${msg.type()}] ${msg.text()}`));
page.on('pageerror', (e) => log(`[pageerror] ${e.message}`));
page.on('websocket', (ws) => {
	log(`[ws open] ${ws.url()}`);
	ws.on('close', () => log(`[ws close] ${ws.url()}`));
	ws.on('framereceived', () => log(`[ws frame<-] ${ws.url().slice(0, 80)}`));
	ws.on('framesent', () => log(`[ws frame->] ${ws.url().slice(0, 80)}`));
	ws.on('socketerror', (err) => log(`[ws error] ${ws.url()}: ${err}`));
});

log('navigating');
await page.goto(`${BASE}/templates/${TID}`, { waitUntil: 'domcontentloaded' });
log('DOMContentLoaded fired');

await page.waitForSelector('.svelte-flow__node', { timeout: 30_000 }).catch(() => log('TIMEOUT waiting for node'));
log('first node visible');

// Linger to capture any late HMR/Yjs activity
await new Promise((r) => setTimeout(r, 5000));
log('done');

await browser.close();
