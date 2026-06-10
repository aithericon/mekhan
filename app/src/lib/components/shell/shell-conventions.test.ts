// Design-system guardrail: route pages must use the shell primitives
// ($lib/components/shell — PageShell / PageHeader / PageTabs / FilterPills)
// instead of hand-rolling their chrome. Scans every +page.svelte /
// +layout.svelte under src/routes at test time and FAILS on:
//
//   1. a raw `<h1`            → use <PageHeader title=... variant=...>
//   2. `mx-auto max-w-…`      → use <PageShell width=...> (one width per
//                                archetype; see README.md in this directory)
//
// Two escape hatches:
//   - CANVAS_ALLOWLIST: permanent, for full-bleed canvas/editor pages and the
//     root app chrome that legitimately own their own layout.
//   - LEGACY_NOT_YET_MIGRATED: pages that predate the shell layer. REMOVE the
//     entry when you migrate a page — a stale entry (listed but clean) also
//     fails, so the list can only shrink. New pages must use the shell.
import { describe, it, expect } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

// Vitest runs with CWD = the app root (where vitest.config.ts lives), and
// under jsdom `import.meta.url` is not a file: URL — so resolve from CWD.
const ROUTES_DIR = join(process.cwd(), 'src', 'routes');

// ── Permanent allowlist ──────────────────────────────────────────────────────
// Full-bleed / bespoke pages that deliberately bypass the shell. Each entry
// needs a reason. Paths are relative to src/routes/.
const CANVAS_ALLOWLIST: Record<string, string> = {
	// Root app chrome: global nav bar, ModeWatcher, auth gate — not a page.
	'+layout.svelte': 'root app chrome, owns the h-screen flex column',
	// Dashboard hero: bespoke Fraunces serif h1 + BrandSpiral decorative motif —
	// a deliberate one-off, not PageHeader material.
	'+page.svelte': 'dashboard hero typography (Fraunces h1) is a deliberate one-off',
	// xyflow provenance graph: h-screen flex column + canvas needing a
	// definite-height unpadded parent; header is a slim toolbar band.
	'catalogue/provenance/[execution_id]/[artifact_id]/+page.svelte':
		'full-bleed xyflow provenance canvas with slim toolbar band'
};

// ── Migration backlog ────────────────────────────────────────────────────────
// Pre-shell pages. Delete the entry when you migrate the page (stale entries
// fail the test). Do NOT add new entries here — new pages use the shell.
const LEGACY_NOT_YET_MIGRATED: string[] = [];

const RAW_H1 = /<h1[\s>]/;
const ADHOC_CONTAINER = /\bmx-auto max-w-/;

function collectRouteFiles(dir: string, base = ''): string[] {
	const out: string[] = [];
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const rel = base ? `${base}/${entry.name}` : entry.name;
		if (entry.isDirectory()) {
			out.push(...collectRouteFiles(join(dir, entry.name), rel));
		} else if (entry.name === '+page.svelte' || entry.name === '+layout.svelte') {
			out.push(rel);
		}
	}
	return out.sort();
}

function violationsOf(rel: string): string[] {
	const src = readFileSync(join(ROUTES_DIR, rel), 'utf8');
	const v: string[] = [];
	if (RAW_H1.test(src)) v.push('raw <h1> — use <PageHeader title=...> instead');
	if (ADHOC_CONTAINER.test(src))
		v.push('ad-hoc `mx-auto max-w-*` container — use <PageShell width=...> instead');
	return v;
}

describe('shell conventions (src/lib/components/shell/README.md)', () => {
	const files = collectRouteFiles(ROUTES_DIR);

	it('finds route files (sanity)', () => {
		expect(files.length).toBeGreaterThan(10);
	});

	it('pages use the shell primitives (no raw <h1>, no ad-hoc max-w container)', () => {
		const offenders = files
			.filter((f) => !(f in CANVAS_ALLOWLIST) && !LEGACY_NOT_YET_MIGRATED.includes(f))
			.map((f) => ({ file: f, violations: violationsOf(f) }))
			.filter((r) => r.violations.length > 0);

		const report = offenders
			.map((r) => `  src/routes/${r.file}\n    - ${r.violations.join('\n    - ')}`)
			.join('\n');
		expect(
			offenders,
			`These route pages bypass the page-shell layer:\n${report}\n` +
				'Build them from $lib/components/shell (see its README.md). ' +
				'Full-bleed canvas pages may be added to CANVAS_ALLOWLIST with a reason.'
		).toEqual([]);
	});

	it('LEGACY_NOT_YET_MIGRATED has no stale entries (remove the entry once migrated)', () => {
		const stale = LEGACY_NOT_YET_MIGRATED.filter(
			(f) => !files.includes(f) || violationsOf(f).length === 0
		);
		expect(
			stale,
			`These entries are clean (or gone) — delete them from LEGACY_NOT_YET_MIGRATED:\n  ${stale.join('\n  ')}`
		).toEqual([]);
	});

	it('allowlists do not overlap', () => {
		const overlap = LEGACY_NOT_YET_MIGRATED.filter((f) => f in CANVAS_ALLOWLIST);
		expect(overlap).toEqual([]);
	});
});
