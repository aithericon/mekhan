<!--
  ArtifactThumb — a compact, fixed-size thumbnail for one catalogue artifact in
  the Insert-media browser. Images are auth-fetched and drawn object-cover (the
  `<img>` can't carry the auth header, same constraint as ArtifactMediaPreview);
  everything else (video / audio / plot-json) shows a typed icon tile so the
  dialog stays light — no blob fetch per video.
-->
<script lang="ts">
	import { catalogueDownloadUrl, type LiveArtifactEntry } from '$lib/api/client';
	import { authFetch } from '$lib/auth/fetch';
	import FileBox from '@lucide/svelte/icons/file-box';
	import Play from '@lucide/svelte/icons/play';
	import AudioLines from '@lucide/svelte/icons/audio-lines';
	import ChartScatter from '@lucide/svelte/icons/chart-scatter';

	let { entry }: { entry: LiveArtifactEntry } = $props();

	const mime = $derived(entry.mime_type ?? '');
	const isImage = $derived(mime.startsWith('image/'));
	const hint = $derived(
		typeof entry.user_metadata?.render_hint === 'string' ? entry.user_metadata.render_hint : null
	);
	const Icon = $derived(
		mime.startsWith('video/') ? Play : mime.startsWith('audio/') ? AudioLines : hint ? ChartScatter : FileBox
	);

	let src = $state<string | null>(null);
	$effect(() => {
		void entry.storage_path;
		src = null;
		if (!isImage || !entry.storage_path) return;
		const controller = new AbortController();
		let url: string | null = null;
		authFetch(catalogueDownloadUrl(entry.storage_path), { signal: controller.signal })
			.then((r) => (r.ok ? r.blob() : Promise.reject(new Error(String(r.status)))))
			.then((b) => {
				url = URL.createObjectURL(b);
				src = url;
			})
			.catch(() => {});
		return () => {
			controller.abort();
			if (url) URL.revokeObjectURL(url);
		};
	});
</script>

<div
	class="flex h-20 w-full items-center justify-center overflow-hidden rounded-md border border-border bg-muted/40"
>
	{#if isImage && src}
		<img {src} alt={entry.name} class="h-full w-full object-cover" loading="lazy" />
	{:else}
		{@const I = Icon}
		<I class="size-6 text-muted-foreground" />
	{/if}
</div>
