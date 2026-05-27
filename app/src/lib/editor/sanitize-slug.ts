/**
 * Coerce a free-text label/id into a Rhai-identifier-safe slug matching
 * `^[a-z][a-z0-9_]*$`. Mirrors the Rust `sanitize_slug` in
 * `service/src/models/template.rs` — keep both in sync, the compiler is
 * authoritative.
 *
 * Used by:
 *   - `NodePropertyPanel` to render the auto-derived slug placeholder.
 *   - `AgentNodeSection` / `ToolMetaSection` to preview the LLM-facing
 *     tool name derived from the target node's label.
 */
export function sanitizeSlug(raw: string): string {
	const s = raw
		.trim()
		.toLowerCase()
		.replace(/[^a-z0-9_]+/g, '_')
		.replace(/_+/g, '_')
		.replace(/^_+|_+$/g, '');
	if (!s) return 'node';
	return /^[a-z]/.test(s) ? s : `n_${s}`;
}
