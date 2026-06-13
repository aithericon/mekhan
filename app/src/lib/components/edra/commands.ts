/**
 * Small command helpers shared by the toolbar + bubble menu. Each takes the
 * live Tiptap `Editor` and runs a chained, focus-restoring command. Keeping
 * these out of the component lets both surfaces share one definition.
 */
import type { Editor } from '@tiptap/core';

export function toggleBold(editor: Editor) {
	editor.chain().focus().toggleBold().run();
}
export function toggleItalic(editor: Editor) {
	editor.chain().focus().toggleItalic().run();
}
export function toggleStrike(editor: Editor) {
	editor.chain().focus().toggleStrike().run();
}
export function toggleCode(editor: Editor) {
	editor.chain().focus().toggleCode().run();
}
export function toggleCodeBlock(editor: Editor) {
	editor.chain().focus().toggleCodeBlock().run();
}
export function toggleBlockquote(editor: Editor) {
	editor.chain().focus().toggleBlockquote().run();
}
export function toggleBulletList(editor: Editor) {
	editor.chain().focus().toggleBulletList().run();
}
export function toggleOrderedList(editor: Editor) {
	editor.chain().focus().toggleOrderedList().run();
}
export function setParagraph(editor: Editor) {
	editor.chain().focus().setParagraph().run();
}
export function toggleHeading(editor: Editor, level: 1 | 2 | 3) {
	editor.chain().focus().toggleHeading({ level }).run();
}
export function setHorizontalRule(editor: Editor) {
	editor.chain().focus().setHorizontalRule().run();
}
export function insertTable(editor: Editor) {
	editor.chain().focus().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run();
}

/**
 * Toggle a link on the current selection. Prompts for a URL when setting; an
 * empty string unsets. Kept dependency-free (uses `window.prompt`) to match the
 * lean vendoring — the host app can later swap in a popover.
 */
export function toggleLink(editor: Editor) {
	const prev = editor.getAttributes('link').href as string | undefined;
	const url = window.prompt('Link URL', prev ?? 'https://');
	if (url === null) return; // cancelled
	if (url === '') {
		editor.chain().focus().extendMarkRange('link').unsetLink().run();
		return;
	}
	editor.chain().focus().extendMarkRange('link').setLink({ href: url }).run();
}
