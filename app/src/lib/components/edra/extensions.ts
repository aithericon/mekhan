/**
 * Trimmed Edra extension set (Tiptap v3) for collaborative page editing.
 *
 * Lean by design (PLAN §5.1): StarterKit + headings/lists/blockquote/HR + link
 * + lowlight code block + tables + placeholder. NO media/image/YouTube, NO
 * slash menu.
 *
 * CRITICAL: `undoRedo: false` (Tiptap v3's renamed StarterKit history) — the
 * Yjs `Collaboration` extension owns undo via `Y.UndoManager`. Leaving Tiptap's
 * native history on corrupts the CRDT. The caller MUST pass a `Y.XmlFragment`
 * (from `doc.getXmlFragment('content')`) so Collaboration binds to the shared
 * doc rather than a local ProseMirror state.
 */
import type { Extension, Mark, Node } from '@tiptap/core';
import type * as Y from 'yjs';
import { StarterKit } from '@tiptap/starter-kit';
import { Collaboration } from '@tiptap/extension-collaboration';
import { Placeholder } from '@tiptap/extension-placeholder';
import { Link } from '@tiptap/extension-link';
import { Table } from '@tiptap/extension-table';
import { TableRow } from '@tiptap/extension-table-row';
import { TableHeader } from '@tiptap/extension-table-header';
import { TableCell } from '@tiptap/extension-table-cell';
import { CodeBlockLowlight } from '@tiptap/extension-code-block-lowlight';
import { createLowlight, common } from 'lowlight';

// Shared lowlight registry — `common` covers the popular languages without the
// full all-languages bundle bloat.
const lowlight = createLowlight(common);

export interface PageExtensionsOptions {
	/** The shared Yjs fragment this editor binds to (`doc.getXmlFragment('content')`). */
	fragment: Y.XmlFragment;
	/** Placeholder shown when the document is empty. */
	placeholder?: string;
}

/**
 * Build the extension array for a collaborative page editor bound to `fragment`.
 *
 * StarterKit's `undoRedo`, `codeBlock`, and `link` are disabled so we can:
 *  - hand undo to Yjs (`undoRedo: false`),
 *  - swap the plain code block for the lowlight-highlighted one,
 *  - configure Link (open-on-click off, safe rel/target).
 */
export function pageExtensions(
	opts: PageExtensionsOptions
): (Extension | Mark | Node)[] {
	return [
		StarterKit.configure({
			// Yjs owns undo — Tiptap's native history MUST be off (CRDT-corrupting).
			undoRedo: false,
			// Replaced by CodeBlockLowlight below.
			codeBlock: false,
			// Replaced by the configured Link below.
			link: false
		}),
		Link.configure({
			openOnClick: false,
			autolink: true,
			defaultProtocol: 'https',
			HTMLAttributes: {
				rel: 'noopener noreferrer nofollow',
				target: '_blank'
			}
		}),
		CodeBlockLowlight.configure({ lowlight }),
		Table.configure({ resizable: true }),
		TableRow,
		TableHeader,
		TableCell,
		Placeholder.configure({
			placeholder: opts.placeholder ?? 'Write something…',
			emptyEditorClass: 'is-editor-empty'
		}),
		Collaboration.configure({ fragment: opts.fragment })
	];
}
