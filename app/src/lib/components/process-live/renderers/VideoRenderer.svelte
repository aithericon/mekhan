<script lang="ts">
	import ArtifactMediaPreview from '$lib/components/catalogue/ArtifactMediaPreview.svelte';
	import type { RendererProps } from './registry';

	// Inline player for a `video/*` artifact (e.g. a recorded `annotated.mp4`).
	// We already render live MJPEG/MSE feeds on edges — a catalogued recording is
	// just a durable file, so play it directly instead of a "no renderer" tile.
	let { entry }: RendererProps = $props();
</script>

<ArtifactMediaPreview
	storagePath={entry.storage_path}
	contentUrl={entry.content_url}
	mimeType={entry.mime_type}
	filename={entry.filename}
	name={entry.name}
	sizeBytes={entry.size_bytes}
/>
