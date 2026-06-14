<script lang="ts">
	// Bordered icon chip for library list / pack-detail rows. Tints the box with
	// the node's accent color (named-glyph case only) and renders the icon itself
	// through the canonical `NodeIcon` (named glyph OR uploaded `asset:` logo).
	import NodeIcon from '$lib/editor/NodeIcon.svelte';
	import { isAssetIcon } from '$lib/api/client';

	let {
		icon,
		color,
		glyphClass = 'size-5',
		boxClass = 'size-9',
		class: className
	}: {
		/** A `presentation.icon` value: `asset:{uuid}` token OR a registry key. */
		icon?: string | null;
		/** Optional accent color (hex / token) used for the named-glyph case. */
		color?: string | null;
		/** Tailwind size class for the icon glyph/image (e.g. `size-5`). */
		glyphClass?: string;
		/** Tailwind size class for the bordered box. */
		boxClass?: string;
		class?: string;
	} = $props();
</script>

<div
	class="flex shrink-0 items-center justify-center overflow-hidden rounded-md border border-border {boxClass} {className ??
		''}"
	style={color && !isAssetIcon(icon) ? `color: ${color}; border-color: ${color}33;` : undefined}
>
	<NodeIcon {icon} class="{glyphClass} object-contain" />
</div>
