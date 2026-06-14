<!--
  ArtifactEmbedView — the Svelte node view for the `artifactEmbed` block.

  Mounted imperatively by the node's addNodeView (Svelte 5 `mount`). Renders one
  of three modes:
    - artifact: a single PINNED catalogue entry, reconstructed from the node's
      snapshot attrs and drawn by the matching renderer (stable — survives even
      if the live buffer has rolled past it).
    - group / all: a LIVE ArtifactsPanel resolved from the shared per-process
      store (optionally filtered to one render bucket), so it stays identical to
      the Process Overview "Media" card and auto-updates.

  `contenteditable="false"` keeps ProseMirror from treating the panel as text.
-->
<script lang="ts">
	import ArtifactsPanel from '$lib/components/process-live/ArtifactsPanel.svelte';
	import ArtifactMediaPreview from '$lib/components/catalogue/ArtifactMediaPreview.svelte';
	import ArtifactProvenance from '$lib/components/catalogue/ArtifactProvenance.svelte';
	import { isShowcaseEntry, pickRenderer, groupKey } from '$lib/components/process-live/renderers/registry';
	import type { LiveArtifactEntry } from '$lib/api/client';
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

	// Live store (group / all modes only). Pinned artifacts render straight from
	// the snapshot attrs and never touch the store.
	const store = $derived(
		context && attrs.processId && attrs.mode !== 'artifact'
			? context.getArtifactStore(attrs.processId)
			: null
	);
	const groupFilter = $derived(attrs.mode === 'group' && attrs.groupKey ? [attrs.groupKey] : undefined);
	const liveCount = $derived.by(() => {
		if (!store) return 0;
		const showcase = store.artifacts.filter(isShowcaseEntry);
		return attrs.mode === 'group'
			? showcase.filter((e) => groupKey(e) === attrs.groupKey).length
			: showcase.length;
	});

	// Reconstruct a minimal catalogue entry for the pinned-artifact renderer +
	// its provenance, from the snapshot attrs captured at insert time.
	function parseUserMeta(): Record<string, unknown> | null {
		if (attrs.userMetaJson) {
			try {
				const m = JSON.parse(attrs.userMetaJson);
				if (m && typeof m === 'object') return m as Record<string, unknown>;
			} catch {
				/* fall through */
			}
		}
		return attrs.renderHint ? { render_hint: attrs.renderHint } : null;
	}
	const pinnedEntry = $derived<LiveArtifactEntry>({
		id: attrs.artifactId,
		artifact_id: attrs.artifactId,
		execution_id: '',
		name: attrs.artifactName,
		category: attrs.category,
		filename: attrs.artifactName,
		mime_type: attrs.mimeType || null,
		storage_path: attrs.storagePath || null,
		size_bytes: attrs.sizeBytes ? Number(attrs.sizeBytes) : null,
		process_step: attrs.processStep || null,
		signal_key: null,
		user_metadata: parseUserMeta(),
		created_at: attrs.createdAt || ''
	} as LiveArtifactEntry);
	const PinnedRenderer = $derived(attrs.mode === 'artifact' ? pickRenderer(pinnedEntry) : null);

	const heading = $derived(
		attrs.caption ||
			(attrs.mode === 'artifact'
				? attrs.artifactName || 'Artifact'
				: attrs.mode === 'group'
					? attrs.groupLabel || 'Media'
					: 'Run media')
	);
</script>

<div contenteditable="false" class="my-3 rounded-xl border border-border bg-card">
	<div class="flex items-center justify-between gap-2 border-b border-border px-3 py-2">
		<div class="flex min-w-0 items-center gap-2 text-sm font-medium">
			<FileBox class="size-4 shrink-0 text-muted-foreground" />
			<span class="truncate">{heading}</span>
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
		{#if attrs.mode === 'artifact'}
			<div class="flex flex-col gap-2">
				{#if PinnedRenderer}
					{@const R = PinnedRenderer}
					<R entry={pinnedEntry} />
				{:else if attrs.storagePath}
					<ArtifactMediaPreview
						storagePath={attrs.storagePath}
						mimeType={attrs.mimeType}
						name={attrs.artifactName}
					/>
				{:else}
					<p class="text-sm text-muted-foreground">This artifact can't be previewed.</p>
				{/if}
				<ArtifactProvenance entry={pinnedEntry} />
			</div>
		{:else if store}
			{#if liveCount === 0}
				<p class="text-sm text-muted-foreground">
					No renderable media yet — it'll appear here as the run produces it.
				</p>
			{:else}
				<ArtifactsPanel {store} renderableOnly showProvenance {groupFilter} />
			{/if}
		{:else}
			<p class="text-sm text-muted-foreground">
				This media block needs a run context and isn't available on this page.
			</p>
		{/if}
	</div>
</div>
