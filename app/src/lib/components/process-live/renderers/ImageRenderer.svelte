<script lang="ts">
	import { catalogueDownloadUrl } from '$lib/api/client';
	import type { LiveArtifactEntry } from '$lib/api/client';

	interface Props {
		entry: LiveArtifactEntry;
	}
	let { entry }: Props = $props();

	const src = $derived(entry.storage_path ? catalogueDownloadUrl(entry.storage_path) : null);
</script>

<div class="flex flex-col gap-2">
	{#if src}
		<img
			{src}
			alt={entry.name}
			class="max-h-[60vh] w-full rounded-lg border border-border bg-card object-contain"
			loading="lazy"
		/>
	{:else}
		<p class="text-sm text-muted-foreground">No storage_path — nothing to render.</p>
	{/if}
	<p class="truncate text-xs text-muted-foreground">
		{entry.filename}
		{#if entry.size_bytes}
			· {(entry.size_bytes / 1024).toFixed(1)} KB
		{/if}
	</p>
</div>
