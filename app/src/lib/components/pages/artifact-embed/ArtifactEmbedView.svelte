<!--
  ArtifactEmbedView — the Svelte node view for the `artifactEmbed` block.

  Mounted imperatively by the node's addNodeView (Svelte 5 `mount`). Resolves the
  shared per-process live store from the run context and renders the EXISTING
  ArtifactsPanel (renderableOnly) so the block stays visually identical to the
  Process Overview "Media" card and auto-updates as the run produces artifacts.

  `contenteditable="false"` keeps ProseMirror from treating the panel as text.
-->
<script lang="ts">
	import ArtifactsPanel from '$lib/components/process-live/ArtifactsPanel.svelte';
	import { isShowcaseEntry } from '$lib/components/process-live/renderers/registry';
	import { Button } from '$lib/components/ui/button';
	import FileBox from '@lucide/svelte/icons/file-box';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import type { ArtifactEmbedContext } from './embed-context';
	import type { ArtifactEmbedAttrs } from './artifact-embed';

	let {
		attrs,
		editable,
		context,
		onDelete
	}: {
		attrs: ArtifactEmbedAttrs;
		editable: boolean;
		context: ArtifactEmbedContext | null;
		onDelete: () => void;
	} = $props();

	const store = $derived(
		context && attrs.processId ? context.getArtifactStore(attrs.processId) : null
	);
	const showcaseCount = $derived(store ? store.artifacts.filter(isShowcaseEntry).length : 0);
</script>

<div contenteditable="false" class="my-3 rounded-xl border border-border bg-card">
	<div class="flex items-center justify-between gap-2 border-b border-border px-3 py-2">
		<div class="flex min-w-0 items-center gap-2 text-sm font-medium">
			<FileBox class="size-4 shrink-0 text-muted-foreground" />
			<span class="truncate">{attrs.caption || 'Run media'}</span>
			{#if attrs.processName}
				<span class="truncate text-muted-foreground">· {attrs.processName}</span>
			{/if}
		</div>
		{#if editable}
			<Button variant="ghost" size="icon-sm" title="Remove block" onclick={onDelete}>
				<Trash2 class="size-4" />
			</Button>
		{/if}
	</div>

	<div class="p-3">
		{#if store}
			{#if showcaseCount === 0}
				<p class="text-sm text-muted-foreground">
					No renderable media yet — it'll appear here as the run produces it.
				</p>
			{:else}
				<ArtifactsPanel {store} renderableOnly />
			{/if}
		{:else}
			<p class="text-sm text-muted-foreground">
				This media block needs a run context and isn't available on this page.
			</p>
		{/if}
	</div>
</div>
