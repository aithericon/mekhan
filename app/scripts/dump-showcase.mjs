#!/usr/bin/env node
// One-shot: dump the showcase graph + per-node files to disk under
// `demos/invoice-processing/`. Run from the `app/` directory:
//
//   node --experimental-strip-types scripts/dump-showcase.mjs
//
// We can't `import` showcase.ts directly because it pulls `$lib/api/client`
// (a Vite alias Node can't resolve). Instead read the file as text, strip
// the imports + type annotations the literal exports don't need, then
// evaluate the residue in a fresh Function scope.
import { promises as fs } from 'node:fs';
import path from 'node:path';

const here = path.dirname(new URL(import.meta.url).pathname);
const appRoot = path.resolve(here, '..');
const repoRoot = path.resolve(appRoot, '..');

const src = await fs.readFile(
	path.join(appRoot, 'src/lib/templates/showcase.ts'),
	'utf8',
);

// Strip:
//   - `import type` lines (compile-time only)
//   - `import { ... } from '$lib/...';` lines (we only need the literal data)
//   - per-name TypeScript annotations on the exports we want to read
// Truncate at the start of `freshShowcaseGraph` — the file's only typed
// non-literal exports (which use generics + Promise<T>) and we don't need
// them. After this point the file is just helper functions for the frontend.
const truncateAt = src.indexOf('function freshShowcaseGraph');
const head = truncateAt > -1 ? src.slice(0, truncateAt) : src;

const stripped = head
	.replace(/^import\s+type\s+[^;]+;\s*$/gm, '')
	.replace(/^import\s+\{[^}]+\}\s+from\s+'[^']+';\s*$/gm, '')
	.replace(/export const showcaseGraph: WorkflowGraph =/, 'const showcaseGraph =')
	.replace(
		/export const showcaseFiles: Record<string, Record<string, string>> =/,
		'const showcaseFiles =',
	);

// Append a small footer that hands the literal exports out via a global.
const wrapped = stripped + `
globalThis.__showcase = { showcaseGraph, showcaseFiles, SHOWCASE_TEMPLATE_NAME, SHOWCASE_TEMPLATE_DESCRIPTION };
`;

// Evaluate as ESM via a tempfile (data: URL hits OS arg-size limits when
// the file is this big). The remaining content is plain JS after the
// stripping above, so no `--experimental-strip-types` needed.
import os from 'node:os';
const tmpFile = path.join(os.tmpdir(), `showcase-dump-${process.pid}.mjs`);
await fs.writeFile(tmpFile, wrapped);
try {
	await import(tmpFile);
} finally {
	await fs.unlink(tmpFile).catch(() => {});
}

const { showcaseGraph, showcaseFiles, SHOWCASE_TEMPLATE_NAME, SHOWCASE_TEMPLATE_DESCRIPTION } =
	globalThis.__showcase;

const outDir = path.join(repoRoot, 'demos/invoice-processing');
const nodesDir = path.join(outDir, 'nodes');
await fs.mkdir(nodesDir, { recursive: true });

// Strip the placeholder trigger node — it carries an unstable id that the
// frontend rewrites at every creation (`freshShowcaseGraph` in showcase.ts).
// For the seeded singleton we mint a stable id once and keep it.
const graph = JSON.parse(JSON.stringify(showcaseGraph));
for (const node of graph.nodes) {
	if (node.id === 'trigger-placeholder') {
		node.id = 'trigger-invoice-api';
		for (const edge of graph.edges) {
			if (edge.source === 'trigger-placeholder') edge.source = 'trigger-invoice-api';
		}
	}
}

await fs.writeFile(
	path.join(outDir, 'graph.json'),
	JSON.stringify(graph, null, 2) + '\n',
);

// `nodes/<id>/<filename>` — real source files, no escaping.
for (const [nodeId, files] of Object.entries(showcaseFiles)) {
	const dir = path.join(nodesDir, nodeId);
	await fs.mkdir(dir, { recursive: true });
	for (const [filename, content] of Object.entries(files)) {
		await fs.writeFile(path.join(dir, filename), content);
	}
}

// `.mekhan.json` — stable template id so the seeder is idempotent across
// dev DB resets, and the test suite can hit the demo by a known id.
await fs.writeFile(
	path.join(outDir, '.mekhan.json'),
	JSON.stringify(
		{
			templateId: '00000000-0000-0000-0000-000000000001',
			name: SHOWCASE_TEMPLATE_NAME,
			description: SHOWCASE_TEMPLATE_DESCRIPTION,
			serverUrl: 'http://localhost:3030',
			lastPull: new Date().toISOString(),
			format: 'json',
		},
		null,
		2,
	) + '\n',
);

console.log(`Wrote demo to ${outDir}`);
