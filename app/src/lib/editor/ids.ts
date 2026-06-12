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
	return `node_${randomUuid().replaceAll('-', '')}`;
}

/**
 * `crypto.randomUUID` exists only in secure contexts (https or localhost) —
 * opening the dev app from another device over plain HTTP
 * (http://192.168.x.x:5173) has `crypto.getRandomValues` but NOT `randomUUID`,
 * which would kill every node/edge creation gesture with a TypeError. Fall
 * back to a getRandomValues-based v4 so insecure contexts keep the same
 * collision safety.
 */
export function randomUuid(): string {
	if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
	const b = crypto.getRandomValues(new Uint8Array(16));
	b[6] = (b[6] & 0x0f) | 0x40; // version 4
	b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
	const h = Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
	return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/**
 * Edge ids keep the readable `e-<source>-<target>-` prefix (handy when
 * eyeballing serialized graphs); the uuid suffix replaces `Date.now()` so two
 * parallel edges between the same pair minted in one tick can't collide.
 * Edge ids never feed slug/identifier derivation, so dashes are fine here.
 */
export function mintEdgeId(source: string, target: string): string {
	return `e-${source}-${target}-${randomUuid().slice(0, 8)}`;
}
