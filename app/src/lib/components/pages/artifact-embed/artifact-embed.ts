/**
 * `artifactEmbed` — the first custom Tiptap node in the vendored Edra editor.
 *
 * An ATOM block (no editable children) inserted into an instance Report. Three
 * embed modes (see ArtifactEmbedView):
 *   - `artifact` — one PINNED artifact (snapshot of its catalogue coordinates),
 *     rendered by the matching renderer. Stable; doesn't change as the run goes.
 *   - `group`    — a LIVE panel filtered to one render bucket (e.g. all
 *     gp-posterior plots, or all images), grouped + scrubbable.
 *   - `all`      — a LIVE panel of every renderable artifact for the process.
 *
 * RENDERING: Svelte 5's `mount`/`unmount` are driven directly from Tiptap's
 * `addNodeView` (no `@tiptap/svelte-renderer` dependency). `ignoreMutation` +
 * `stopEvent` keep ProseMirror's hands off the interactive panel.
 *
 * PERSISTENCE: attributes round-trip through ProseMirror's schema → y-prosemirror
 * → the page's Yjs doc natively; the per-attribute data-* HTML hooks are only
 * for clipboard copy/paste.
 */
import { Node, mergeAttributes, type Attributes } from '@tiptap/core';
import { mount, unmount } from 'svelte';
import ArtifactEmbedView from './ArtifactEmbedView.svelte';
import type { ArtifactEmbedContext } from './embed-context';

export interface ArtifactEmbedOptions {
	/** Run context; null on pages with no run (block renders a placeholder). */
	context: ArtifactEmbedContext | null;
}

export type ArtifactEmbedMode = 'all' | 'group' | 'artifact';

export interface ArtifactEmbedAttrs {
	processId: string | null;
	processName: string;
	mode: ArtifactEmbedMode;
	/** group mode: the render-bucket key (e.g. `hint:gp-posterior`, `mime:image`). */
	groupKey: string;
	groupLabel: string;
	/** artifact mode: pinned snapshot of one catalogue entry. */
	artifactId: string;
	artifactName: string;
	storagePath: string;
	mimeType: string;
	renderHint: string;
	category: string;
	processStep: string;
	caption: string;
}

// Every attribute is a serializable string (processId nullable). Built from one
// list so the schema, the data-* round-trip, and the TS type stay in lockstep.
const STRING_ATTRS: (keyof ArtifactEmbedAttrs)[] = [
	'processId',
	'processName',
	'mode',
	'groupKey',
	'groupLabel',
	'artifactId',
	'artifactName',
	'storagePath',
	'mimeType',
	'renderHint',
	'category',
	'processStep',
	'caption'
];

const dataName = (key: string) => 'data-' + key.replace(/[A-Z]/g, (m) => '-' + m.toLowerCase());

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
		const attrs: Attributes = {};
		for (const key of STRING_ATTRS) {
			const data = dataName(key);
			const fallback = key === 'mode' ? 'all' : key === 'processId' ? null : '';
			attrs[key] = {
				default: fallback,
				parseHTML: (el: HTMLElement) => el.getAttribute(data) ?? fallback,
				renderHTML: (a: Record<string, unknown>) => (a[key] ? { [data]: a[key] } : {})
			};
		}
		return attrs;
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
				update: (updatedNode) => updatedNode.type.name === 'artifactEmbed',
				ignoreMutation: () => true,
				stopEvent: () => true,
				destroy: () => {
					void unmount(view);
				}
			};
		};
	}
});
