import { describe, it, expect } from 'vitest';
import { generateHTML, generateJSON, type JSONContent } from '@tiptap/core';
import { StarterKit } from '@tiptap/starter-kit';
import { ArtifactEmbed } from './artifact-embed';

// Schema-only round-trip (no live editor / node view): proves the node's
// attributes survive ProseMirror serialization, which is what y-prosemirror
// persists into the page's Yjs doc and what clipboard copy/paste relies on.
const extensions = [StarterKit.configure({ undoRedo: false }), ArtifactEmbed];

describe('ArtifactEmbed node', () => {
	it('serializes an "all" (live) embed to data-*', () => {
		const doc: JSONContent = {
			type: 'doc',
			content: [
				{
					type: 'artifactEmbed',
					attrs: { processId: 'proc-1', processName: 'Simulate', mode: 'all' }
				}
			]
		};
		const html = generateHTML(doc, extensions);
		expect(html).toContain('data-type="artifact-embed"');
		expect(html).toContain('data-process-id="proc-1"');
		expect(html).toContain('data-process-name="Simulate"');
		expect(html).toContain('data-mode="all"');
	});

	it('serializes a pinned-artifact embed with its snapshot coordinates', () => {
		const doc: JSONContent = {
			type: 'doc',
			content: [
				{
					type: 'artifactEmbed',
					attrs: {
						processId: 'proc-1',
						mode: 'artifact',
						artifactId: 'art-9',
						artifactName: 'gp_final.png',
						storagePath: 'artifacts/exec-1/plot/gp_final.png',
						mimeType: 'image/png',
						renderHint: 'gp-posterior'
					}
				}
			]
		};
		const html = generateHTML(doc, extensions);
		expect(html).toContain('data-mode="artifact"');
		expect(html).toContain('data-artifact-id="art-9"');
		expect(html).toContain('data-storage-path="artifacts/exec-1/plot/gp_final.png"');
		expect(html).toContain('data-mime-type="image/png"');
		expect(html).toContain('data-render-hint="gp-posterior"');
	});

	it('parses a group embed back into attributes (clipboard round-trip)', () => {
		const html =
			'<div data-type="artifact-embed" data-process-id="proc-9" data-mode="group" data-group-key="mime:image" data-group-label="image"></div>';
		const json = generateJSON(html, extensions) as { content?: JSONContent[] };
		const node = json.content?.find((n) => n.type === 'artifactEmbed');
		expect(node).toBeTruthy();
		expect(node?.attrs?.processId).toBe('proc-9');
		expect(node?.attrs?.mode).toBe('group');
		expect(node?.attrs?.groupKey).toBe('mime:image');
		expect(node?.attrs?.groupLabel).toBe('image');
	});

	it('defaults mode to "all" when unspecified', () => {
		const json = generateJSON(
			'<div data-type="artifact-embed" data-process-id="p"></div>',
			extensions
		) as { content?: JSONContent[] };
		const node = json.content?.find((n) => n.type === 'artifactEmbed');
		expect(node?.attrs?.mode).toBe('all');
	});

	it('is a block-group atom that can sit between paragraphs', () => {
		expect(ArtifactEmbed.name).toBe('artifactEmbed');
		const doc: JSONContent = {
			type: 'doc',
			content: [
				{ type: 'paragraph', content: [{ type: 'text', text: 'before' }] },
				{ type: 'artifactEmbed', attrs: { processId: 'p', mode: 'all' } },
				{ type: 'paragraph', content: [{ type: 'text', text: 'after' }] }
			]
		};
		const html = generateHTML(doc, extensions);
		expect(html).toContain('before');
		expect(html).toContain('after');
		expect(html).toContain('data-type="artifact-embed"');
	});
});
