<script lang="ts">
	import ArtifactMediaPreview from '$lib/components/catalogue/ArtifactMediaPreview.svelte';
	import type { RendererProps } from './registry';

	// Inline player for an `audio/*` artifact (e.g. a recorded wav/mp3).
	let { entry }: RendererProps = $props();
</script>

<ArtifactMediaPreview
	storagePath={entry.storage_path}
	mimeType={entry.mime_type}
	filename={entry.filename}
	name={entry.name}
	sizeBytes={entry.size_bytes}
/>
