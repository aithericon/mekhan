<script lang="ts">
	// Single bare renderer for a library / vendor node's `presentation.icon`.
	//
	// A `presentation.icon` value is one of two kinds:
	//   - `asset:{uuid}` → a CUSTOM uploaded logo image, served auth-gated at
	//     `GET /api/v1/library/icons/{uuid}`. Auth is the same-origin
	//     `mekhan_session` HttpOnly cookie, which the browser attaches to an
	//     `<img src>` automatically (dev_noop and Zitadel alike — see
	//     `$lib/auth/fetch`), so a plain `<img>` is all we need.
	//   - anything else → a NAMED icon-registry key resolved to a bundled Svelte
	//     glyph component by `resolveNodeIcon` (with its own generic fallback).
	//
	// This is the canonical renderer; `LibraryIconBox` wraps it in a bordered
	// chip for list/detail rows, and SubWorkflowCardIcon bridges it through
	// WorkflowNodeCard's class-only icon slot.
	import { libraryIconUrl } from '$lib/api/client';
	import { resolveNodeIcon } from '$lib/editor/icon-registry';

	let { icon, class: className }: { icon?: string | null; class?: string } = $props();

	const assetUrl = $derived(libraryIconUrl(icon));
	const Glyph = $derived(resolveNodeIcon(icon));
</script>

{#if assetUrl}
	<img src={assetUrl} alt="" class={className} />
{:else}
	<Glyph class={className} />
{/if}
