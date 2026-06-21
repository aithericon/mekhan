<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import Download from '@lucide/svelte/icons/download';
	import FileIcon from '@lucide/svelte/icons/file';
	import { catalogueDownloadUrl } from '$lib/api/client';
	import { authFetch } from '$lib/auth/fetch';

	// A self-contained preview for ONE catalogued artifact, given only its
	// catalogue coordinates (`storage_path` + `mime_type`). Used by both the
	// live artifact viewer (`process-live/renderers/*`) and the step-output
	// envelope (`output-renderers/ArtifactManifest.svelte`) so a recorded mp4 /
	// wav / png renders inline everywhere instead of degrading to a download tile.
	//
	// `<img>`/`<video>`/`<audio>` can't carry an Authorization header, so the
	// catalogue download URL 401s under Zitadel auth. We fetch the bytes via
	// `authFetch` and hand the tag an object URL instead — same approach as the
	// original `ImageRenderer`. The object URL is revoked on change/teardown.
	interface Props {
		storagePath: string | null;
		mimeType: string | null;
		/**
		 * Pre-resolved by-reference fetch URL (bytes served by content hash for a
		 * crawled / file-server entry). When set it wins over `storagePath`; when
		 * omitted the component keeps its original storage-path behavior.
		 */
		contentUrl?: string | null;
		filename?: string | null;
		name?: string | null;
		sizeBytes?: number | null;
	}
	let { storagePath, contentUrl, mimeType, filename, name, sizeBytes }: Props = $props();

	const kind = $derived<'image' | 'video' | 'audio' | null>(
		(mimeType ?? '').startsWith('image/')
			? 'image'
			: (mimeType ?? '').startsWith('video/')
				? 'video'
				: (mimeType ?? '').startsWith('audio/')
					? 'audio'
					: null
	);

	// Bytes come from the by-reference URL if given, else the platform store.
	const fetchUrl = $derived(contentUrl ?? (storagePath ? catalogueDownloadUrl(storagePath) : null));
	const downloadUrl = $derived(fetchUrl);
	// Prefer `name` over `filename`: the catalogue's `filename` can carry a
	// spurious trailing `.json` (artifact-pipeline naming quirk), so as a last
	// line of defence strip a `.json` suffix that doesn't match the real MIME.
	const label = $derived.by(() => {
		let l = name ?? filename ?? storagePath?.split('/').pop() ?? 'artifact';
		if (l.endsWith('.json') && (mimeType ?? '') !== 'application/json') {
			l = l.slice(0, -'.json'.length);
		}
		return l;
	});

	let src = $state<string | null>(null);
	let error = $state<string | null>(null);
	// Set when the browser can decode the bytes but not the codec (e.g. an mp4
	// recorded with the mp4v / MPEG-4 Part 2 codec, which no browser plays in
	// <video>). The bytes are fine — only in-browser playback fails — so we keep
	// the download link and explain rather than showing the browser's default
	// "no supported source" frame.
	let playbackError = $state(false);

	$effect(() => {
		// Re-run whenever the underlying file changes.
		void fetchUrl;
		src = null;
		error = null;
		playbackError = false;
		if (!kind || !fetchUrl) return;
		const controller = new AbortController();
		let objectUrl: string | null = null;
		authFetch(fetchUrl, { signal: controller.signal })
			.then((r) => {
				if (!r.ok) throw new Error(`fetch failed: ${r.status}`);
				return r.blob();
			})
			.then((b) => {
				objectUrl = URL.createObjectURL(b);
				src = objectUrl;
			})
			.catch((e) => {
				if (controller.signal.aborted) return;
				error = e instanceof Error ? e.message : String(e);
			});
		return () => {
			controller.abort();
			if (objectUrl) URL.revokeObjectURL(objectUrl);
		};
	});

	function formatBytes(b: number | null | undefined): string | null {
		if (b === undefined || b === null) return null;
		if (b < 1024) return `${b} B`;
		if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
		return `${(b / (1024 * 1024)).toFixed(1)} MB`;
	}
	const sizeLabel = $derived(formatBytes(sizeBytes));
</script>

<div class="flex min-w-0 flex-col gap-2">
	{#if kind}
		{#if error}
			<p class="text-sm text-red-500">Failed to load preview: {error}</p>
		{:else if src}
			{#if kind === 'image'}
				<img
					{src}
					alt={label}
					class="max-h-[60vh] w-auto max-w-2xl rounded-lg border border-border bg-card object-contain"
					loading="lazy"
				/>
			{:else if kind === 'video'}
				{#if playbackError}
					<div
						class="flex max-w-2xl flex-col gap-1 rounded-lg border border-border bg-muted/30 p-3 text-sm text-muted-foreground"
					>
						<span class="font-medium text-foreground">Not playable in the browser</span>
						This video's codec isn't supported for in-browser playback (e.g. an mp4
						written with the <code>mp4v</code> codec). The file itself is intact — use the
						download link below to view it.
					</div>
				{:else}
					<!-- svelte-ignore a11y_media_has_caption -->
					<video
						controls
						{src}
						onerror={() => (playbackError = true)}
						class="max-h-[60vh] w-full max-w-2xl rounded-lg border border-border bg-black"
					>
						Your browser does not support the video element.
					</video>
				{/if}
			{:else if playbackError}
				<div
					class="flex max-w-2xl flex-col gap-1 rounded-lg border border-border bg-muted/30 p-3 text-sm text-muted-foreground"
				>
					<span class="font-medium text-foreground">Not playable in the browser</span>
					This audio's codec isn't supported for in-browser playback. Use the download link
					below to listen.
				</div>
			{:else}
				<!-- svelte-ignore a11y_media_has_caption -->
				<audio controls {src} onerror={() => (playbackError = true)} class="w-full max-w-2xl">
					Your browser does not support the audio element.
				</audio>
			{/if}
		{:else}
			<div class="text-sm text-muted-foreground">Loading preview…</div>
		{/if}
	{/if}

	<div class="flex flex-wrap items-center gap-2 text-sm">
		{#if downloadUrl}
			<a
				href={downloadUrl}
				download={label}
				class="group inline-flex items-center gap-1.5 rounded-md border border-border bg-muted/30 px-2.5 py-1 transition-colors hover:bg-accent hover:text-accent-foreground"
				title="Download {label}"
			>
				<Download class="size-3.5 shrink-0 text-muted-foreground group-hover:text-foreground" />
				<span class="truncate font-medium">{label}</span>
			</a>
		{:else}
			<span class="inline-flex items-center gap-1.5 text-muted-foreground">
				<FileIcon class="size-3.5 shrink-0" />
				<span class="truncate font-medium">{label}</span>
			</span>
		{/if}
		{#if mimeType}
			<Badge variant="outline" class="font-mono text-sm">{mimeType}</Badge>
		{/if}
		{#if sizeLabel}
			<span class="text-muted-foreground">{sizeLabel}</span>
		{/if}
		{#if !fetchUrl}
			<span class="text-muted-foreground italic">upload pending…</span>
		{/if}
	</div>
</div>
