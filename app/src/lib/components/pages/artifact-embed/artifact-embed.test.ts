import { describe, it, expect } from 'vitest';
import { generateHTML, generateJSON, type JSONContent } from '@tiptap/core';
import { StarterKit } from '@tiptap/starter-kit';
import { ArtifactEmbed } from './artifact-embed';

// Schema-only round-trip (no live editor / node view): proves the node's
// attributes survive ProseMirror serialization, which is what y-prosemirror
// persists into the page's Yjs doc and what clipboard copy/paste relies on.
const extensions = [StarterKit.configure({ undoRedo: false }), ArtifactEmbed];

describe('ArtifactEmbed node', () => {
	it('serializes attributes to data-* in HTML', () => {
		const doc: JSONContent = {
			type: 'doc',
			content: [
				{
					type: 'artifactEmbed',
					attrs: { processId: 'proc-1', processName: 'Simulate', caption: 'Final renders' }
				}
			]
		};
		const html = generateHTML(doc, extensions);
		expect(html).toContain('data-type="artifact-embed"');
		expect(html).toContain('data-process-id="proc-1"');
		expect(html).toContain('data-process-name="Simulate"');
		expect(html).toContain('data-caption="Final renders"');
	});

	it('parses data-* back into attributes (clipboard round-trip)', () => {
		const html =
			'<div data-type="artifact-embed" data-process-id="proc-9" data-process-name="Run" data-caption="Plots"></div>';
		const json = generateJSON(html, extensions) as { content?: JSONContent[] };
		const node = json.content?.find((n) => n.type === 'artifactEmbed');
		expect(node).toBeTruthy();
		expect(node?.attrs?.processId).toBe('proc-9');
		expect(node?.attrs?.processName).toBe('Run');
		expect(node?.attrs?.caption).toBe('Plots');
	});

	it('is a block-group atom (no editable children, can sit between paragraphs)', () => {
		expect(ArtifactEmbed.name).toBe('artifactEmbed');
		const doc: JSONContent = {
			type: 'doc',
			content: [
				{ type: 'paragraph', content: [{ type: 'text', text: 'before' }] },
				{ type: 'artifactEmbed', attrs: { processId: 'p', processName: '', caption: '' } },
				{ type: 'paragraph', content: [{ type: 'text', text: 'after' }] }
			]
		};
		// Round-trips without throwing → the node is a valid block in the schema.
		const html = generateHTML(doc, extensions);
		expect(html).toContain('before');
		expect(html).toContain('after');
		expect(html).toContain('data-type="artifact-embed"');
	});
});
