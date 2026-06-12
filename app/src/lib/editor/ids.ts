/**
 * Collision-safe id minting for canvas nodes and edges.
 *
 * The previous `node-${Date.now()}` scheme collides whenever two collaborators
 * drop a node in the same millisecond — and trivially when one client mints
 * several ids in one tick (paste of a multi-node clipboard).
 *
 * Node ids feed the compiler's DEFAULT slug derivation: a node without an
 * explicit `slug` gets `sanitize_slug(id)` (service/src/models/template/
 * graph.rs), and that slug is the Rhai-identifier namespace authors reference
 * as `<slug>.<field>`. So the minted id is kept identifier-safe already
 * (`^[a-z][a-z0-9_]*$`): lowercase hex uuid with the dashes stripped, behind
 * a `node_` prefix — the derived slug equals the id verbatim, no surprising
 * sanitizer rewrites.
 */
export function mintNodeId(): string {
	return `node_${crypto.randomUUID().replaceAll('-', '')}`;
}

/**
 * Edge ids keep the readable `e-<source>-<target>-` prefix (handy when
 * eyeballing serialized graphs); the uuid suffix replaces `Date.now()` so two
 * parallel edges between the same pair minted in one tick can't collide.
 * Edge ids never feed slug/identifier derivation, so dashes are fine here.
 */
export function mintEdgeId(source: string, target: string): string {
	return `e-${source}-${target}-${crypto.randomUUID().slice(0, 8)}`;
}
