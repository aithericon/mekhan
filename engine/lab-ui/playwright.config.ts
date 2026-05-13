import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
	testDir: './tests',
	fullyParallel: true,
	forbidOnly: !!process.env.CI,
	retries: process.env.CI ? 2 : 0,
	workers: process.env.CI ? 1 : undefined,
	reporter: 'html',
	timeout: 60000,
	use: {
		baseURL: 'http://localhost:5173',
		trace: 'on-first-retry'
	},
	projects: [
		{
			name: 'chromium',
			use: { ...devices['Desktop Chrome'] }
		}
	],
	webServer: [
		{
			command: 'cargo run --manifest-path ../Cargo.toml --bin core-engine',
			url: 'http://localhost:3030/api/topology',
			reuseExistingServer: true,
			timeout: 120000,
			stdout: 'pipe',
			stderr: 'pipe'
		},
		{
			command: 'deno task dev -- --port 5173',
			url: 'http://localhost:5173',
			reuseExistingServer: true,
			timeout: 30000,
			stdout: 'pipe',
			stderr: 'pipe'
		}
	]
});
