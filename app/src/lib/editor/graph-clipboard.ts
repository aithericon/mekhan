/**
 * In-memory module clipboard for canvas copy/paste — deliberately NOT the
 * system clipboard (the payload contains Y.Map config snapshots + file text;
 * serializing those through the OS clipboard buys nothing and risks pasting
 * graph JSON into unrelated apps). Surviving the editor component is a
 * feature: copy in one template, paste into another (ids are re-minted on
 * paste, so cross-template paste is safe).
 */
import type { GraphClipboard } from '$lib/yjs/graph-binding.svelte';

let clip: GraphClipboard | null = null;
let pastes = 0;

export function setClipboard(c: GraphClipboard): void {
	clip = c;
	pastes = 0;
}

export function getClipboard(): GraphClipboard | null {
	return clip;
}

/**
 * Offset for the NEXT paste of the current clipboard: 24px down-right per
 * paste, compounding, so repeated Cmd+V fans the clones out instead of
 * stacking them all on one spot. Re-copying resets the fan.
 */
export function nextPasteOffset(): { x: number; y: number } {
	pastes += 1;
	return { x: 24 * pastes, y: 24 * pastes };
}
