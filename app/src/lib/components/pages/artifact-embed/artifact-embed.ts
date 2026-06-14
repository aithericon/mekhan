/**
 * `artifactEmbed` — the first custom Tiptap node in the vendored Edra editor.
 *
 * An ATOM block (no editable children) that references a run's process and
 * renders a LIVE panel of that process's renderable artifacts. The reference is
 * a tiny set of serializable attributes (`processId` + display strings) — the
 * actual media is fetched at render time from the shared live store, so the
 * block auto-updates as the run produces more output.
 *
 * RENDERING: Svelte 5's `mount`/`unmount` are driven directly from Tiptap's
 * `addNodeView` (no `@tiptap/svelte-renderer` dependency). `ignoreMutation` +
 * `stopEvent` keep ProseMirror's hands off the interactive panel (scrubber,
 * lightbox, delete button) inside the node view.
 *
 * PERSISTENCE: attributes round-trip through ProseMirror's schema → y-prosemirror
 * → the page's Yjs doc natively; the per-attribute parse/render HTML hooks are
 * only for clipboard copy/paste.
 */
import { Node, mergeAttributes } from '@tiptap/core';
import { mount, unmount } from 'svelte';
import ArtifactEmbedView from './ArtifactEmbedView.svelte';
import type { ArtifactEmbedContext } from './embed-context';

export interface ArtifactEmbedOptions {
	/** Run context; null on pages with no run (block renders a placeholder). */
	context: ArtifactEmbedContext | null;
}

export interface ArtifactEmbedAttrs {
	processId: string | null;
	processName: string;
	caption: string;
}

export const ArtifactEmbed = Node.create<ArtifactEmbedOptions>({
	name: 'artifactEmbed',
	group: 'block',
	atom: true,
	selectable: true,
	draggable: false,

	addOptions() {
		return { context: null };
	},

	addAttributes() {
		return {
			processId: {
				default: null,
				parseHTML: (el) => el.getAttribute('data-process-id'),
				renderHTML: (attrs) =>
					attrs.processId ? { 'data-process-id': attrs.processId } : {}
			},
			processName: {
				default: '',
				parseHTML: (el) => el.getAttribute('data-process-name') ?? '',
				renderHTML: (attrs) =>
					attrs.processName ? { 'data-process-name': attrs.processName } : {}
			},
			caption: {
				default: '',
				parseHTML: (el) => el.getAttribute('data-caption') ?? '',
				renderHTML: (attrs) => (attrs.caption ? { 'data-caption': attrs.caption } : {})
			}
		};
	},

	parseHTML() {
		return [{ tag: 'div[data-type="artifact-embed"]' }];
	},

	renderHTML({ HTMLAttributes }) {
		return ['div', mergeAttributes(HTMLAttributes, { 'data-type': 'artifact-embed' })];
	},

	addNodeView() {
		const context = this.options.context;
		return ({ node, editor, getPos }) => {
			const dom = document.createElement('div');
			dom.setAttribute('data-type', 'artifact-embed');
			const view = mount(ArtifactEmbedView, {
				target: dom,
				props: {
					attrs: node.attrs as ArtifactEmbedAttrs,
					editable: editor.isEditable,
					context,
					onDelete: () => {
						if (typeof getPos !== 'function') return;
						const pos = getPos();
						if (pos == null) return;
						editor
							.chain()
							.focus()
							.deleteRange({ from: pos, to: pos + node.nodeSize })
							.run();
					}
				}
			});
			return {
				dom,
				// Atom node — accept same-type updates without rebuilding the view
				// (attributes are immutable post-insert in v1).
				update: (updatedNode) => updatedNode.type.name === 'artifactEmbed',
				// The Svelte subtree owns its DOM; don't let ProseMirror re-read it.
				ignoreMutation: () => true,
				// Interactive widget — PM must not hijack clicks/keys inside it.
				stopEvent: () => true,
				destroy: () => {
					void unmount(view);
				}
			};
		};
	}
});
