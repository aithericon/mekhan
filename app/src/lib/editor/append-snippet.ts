/**
 * Shared "append snippet to text field" utility.
 *
 * Many section components expose an InsertRefButton whose `oninsert` callback
 * appends a picked reference snippet to an existing field value. The pattern
 * is always the same:
 *
 *   curr ? `${curr}<sep>${snippet}` : snippet
 *
 * The separator is a single space for most fields (Markdown/Tera template
 * strings). SmtpConfigPanel uses an empty separator because it appends Tera
 * snippets directly adjacent to the cursor position.
 *
 * @param curr    Current field value (may be undefined/null — treated as '').
 * @param snippet Snippet to append.
 * @param sep     Separator inserted between `curr` and `snippet` when `curr`
 *                is non-empty. Defaults to `' '` (single space).
 */
export function appendSnippet(
	curr: string | null | undefined,
	snippet: string,
	sep = ' '
): string {
	const base = curr ?? '';
	return base.length > 0 ? `${base}${sep}${snippet}` : snippet;
}
