#!/usr/bin/env node
// lint-ui.mjs — design-system guardrails.
//
// Scans app/src outside lib/components/ui/ and flags:
//   1. Literal Tailwind palette colors (bg-/text-/border-/ring-{green|red|amber|
//      blue|emerald|rose|teal|purple|indigo|orange|yellow|pink|cyan|lime|sky|
//      fuchsia|violet|slate|zinc|neutral|stone|gray}-NNN). Use theme tokens
//      (--success, --warning, --info, --destructive, --primary, etc.) instead.
//   2. Raw <input> / <textarea> in .svelte files. Use <Input> / <Textarea>.
//
// Allow-listed by placing a comment on the offending line or the line above:
//   <!-- ui-allow: reason -->   (in markup)
//   // ui-allow: reason           (in script blocks)
//
// Baseline mode: existing violations are captured in lint-ui.baseline.json
// so the script only fails on NEW violations. Refresh the baseline with:
//   pnpm lint:ui --update-baseline
//
// Exits 1 on violations not in the baseline.

import { readdirSync, readFileSync, statSync, writeFileSync, existsSync } from 'node:fs';
import { join, relative, sep, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const ROOT = join(SCRIPT_DIR, '..', 'src');
const REPO = join(SCRIPT_DIR, '..');
const UI_DIR = join(ROOT, 'lib', 'components', 'ui') + sep;
const BASELINE_PATH = join(SCRIPT_DIR, 'lint-ui.baseline.json');

const PALETTE_FAMILIES = [
	'slate', 'gray', 'zinc', 'neutral', 'stone',
	'red', 'orange', 'amber', 'yellow', 'lime',
	'green', 'emerald', 'teal', 'cyan', 'sky',
	'blue', 'indigo', 'violet', 'purple', 'fuchsia',
	'pink', 'rose',
];
const PALETTE_RE = new RegExp(
	`(?:bg|text|border|ring|from|to|via|fill|stroke|outline|divide|placeholder|accent|caret|decoration|shadow)-(?:${PALETTE_FAMILIES.join('|')})-\\d{2,3}\\b`,
);
const RAW_INPUT_RE = /<(input|textarea)\b/;
const ALLOW_RE = /ui-allow:/;

const SKIP_DIRS = new Set(['node_modules', '.svelte-kit', '.git', 'build', 'dist']);
const SCAN_EXT = new Set(['.svelte', '.ts', '.tsx', '.js', '.jsx', '.css']);

const args = new Set(process.argv.slice(2));
const updateBaseline = args.has('--update-baseline');

function walk(dir, files = []) {
	for (const entry of readdirSync(dir)) {
		if (SKIP_DIRS.has(entry)) continue;
		const full = join(dir, entry);
		const st = statSync(full);
		if (st.isDirectory()) walk(full, files);
		else {
			const dot = entry.lastIndexOf('.');
			if (dot >= 0 && SCAN_EXT.has(entry.slice(dot))) files.push(full);
		}
	}
	return files;
}

function lineAllowed(lines, idx) {
	if (ALLOW_RE.test(lines[idx])) return true;
	if (idx > 0 && ALLOW_RE.test(lines[idx - 1])) return true;
	return false;
}

const violations = [];

for (const file of walk(ROOT)) {
	if (file.startsWith(UI_DIR)) continue;
	const text = readFileSync(file, 'utf8');
	const lines = text.split('\n');
	const rel = relative(REPO, file);

	for (let i = 0; i < lines.length; i++) {
		const line = lines[i];
		const paletteMatch = line.match(PALETTE_RE);
		if (paletteMatch && !lineAllowed(lines, i)) {
			violations.push({ file: rel, rule: 'palette', detail: paletteMatch[0] });
		}
		if (file.endsWith('.svelte')) {
			const rawMatch = line.match(RAW_INPUT_RE);
			if (rawMatch && !lineAllowed(lines, i)) {
				violations.push({ file: rel, rule: 'raw-form-element', detail: `<${rawMatch[1]}>` });
			}
		}
	}
}

// Baselining: count by (file, rule, detail). New violations are those that
// push a (file, rule, detail) bucket above its baseline count. This is
// resilient to line-number drift but still catches "added another input to
// the same file."
function bucketize(items) {
	const buckets = new Map();
	for (const v of items) {
		const key = `${v.file}::${v.rule}::${v.detail}`;
		buckets.set(key, (buckets.get(key) ?? 0) + 1);
	}
	return buckets;
}

if (updateBaseline) {
	const buckets = bucketize(violations);
	const serialized = Object.fromEntries([...buckets.entries()].sort());
	writeFileSync(BASELINE_PATH, JSON.stringify(serialized, null, 2) + '\n');
	console.log(`lint:ui — baseline updated (${violations.length} violation(s) captured).`);
	process.exit(0);
}

let baseline = {};
if (existsSync(BASELINE_PATH)) {
	baseline = JSON.parse(readFileSync(BASELINE_PATH, 'utf8'));
}

const current = bucketize(violations);
const newViolations = [];
for (const [key, count] of current) {
	const baseCount = baseline[key] ?? 0;
	if (count > baseCount) {
		const [file, rule, detail] = key.split('::');
		for (let i = 0; i < count - baseCount; i++) {
			newViolations.push({ file, rule, detail });
		}
	}
}

const baselineTotal = Object.values(baseline).reduce((a, b) => a + b, 0);

if (newViolations.length === 0) {
	console.log(`lint:ui — clean. (${violations.length} baselined, ${baselineTotal} expected)`);
	process.exit(0);
}

const byRule = new Map();
for (const v of newViolations) {
	if (!byRule.has(v.rule)) byRule.set(v.rule, []);
	byRule.get(v.rule).push(v);
}

console.log(`lint:ui — ${newViolations.length} NEW violation(s) above baseline:\n`);
for (const [rule, items] of byRule) {
	console.log(`${rule} (${items.length} new):`);
	for (const v of items.slice(0, 50)) {
		console.log(`  ${v.file}  ${v.detail}`);
	}
	if (items.length > 50) console.log(`  … ${items.length - 50} more`);
	console.log();
}
console.log(`Use theme tokens (--success, --warning, --info, --destructive, --primary) or extend a primitive in app/src/lib/components/ui/. See ui/README.md.`);
console.log(`If a violation is genuinely needed, add an inline comment containing "ui-allow: <reason>" on the line or line above.`);
console.log(`To re-baseline after a deliberate refactor: pnpm lint:ui --update-baseline`);
process.exit(1);
