<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import type { RendererProps } from './types';

	// Inline media player for a playable media source — either a file-ref object
	// (`{url, content_type, filename?}` whose `content_type` is audio/* | video/*
	// | image/*) or a `data:` URL carrying the same media MIME. The dispatch
	// predicate (`matchesMedia` in index.ts) guarantees one of those two shapes,
	// so this component just normalises to `{src, contentType, filename}` and
	// renders the right tag. The visual frame mirrors `ide/ImageViewer.svelte`
	// (header strip + checkerboard/centered body) for parity.
	let { value }: RendererProps = $props();

	type Media = { src: string; contentType: string; filename: string | null };

	const media = $derived.by<Media | null>(() => {
		if (typeof value === 'string') {
			// `data:<mime>;...,<payload>` — pull the MIME between `data:` and the
			// first `;` or `,`.
			const m = /^data:([^;,]+)/.exec(value);
			if (!m) return null;
			return { src: value, contentType: m[1], filename: null };
		}
		if (value && typeof value === 'object' && !Array.isArray(value)) {
			const obj = value as Record<string, unknown>;
			if (typeof obj.url === 'string' && typeof obj.content_type === 'string') {
				const filename =
					typeof obj.filename === 'string'
						? obj.filename
						: (obj.url.split('/').pop() ?? null);
				return { src: obj.url, contentType: obj.content_type, filename };
			}
		}
		return null;
	});

	const kind = $derived<'audio' | 'video' | 'image' | null>(
		media
			? media.contentType.startsWith('audio/')
				? 'audio'
				: media.contentType.startsWith('video/')
					? 'video'
					: media.contentType.startsWith('image/')
						? 'image'
						: null
			: null
	);
</script>

{#if media && kind}
	<div class="flex min-w-0 flex-col gap-2 rounded-md border border-border bg-muted/30 p-3">
		<div class="flex flex-wrap items-center gap-2 text-sm">
			{#if media.filename}
				<span class="truncate font-medium text-foreground">{media.filename}</span>
			{/if}
			<Badge variant="outline" class="font-mono text-sm">{media.contentType}</Badge>
		</div>
		{#if kind === 'audio'}
			<!-- svelte-ignore a11y_media_has_caption -->
			<audio controls src={media.src} class="w-full">
				Your browser does not support the audio element.
			</audio>
		{:else if kind === 'video'}
			<!-- svelte-ignore a11y_media_has_caption -->
			<video controls src={media.src} class="max-h-[60vh] w-full rounded">
				Your browser does not support the video element.
			</video>
		{:else}
			<img
				src={media.src}
				alt={media.filename ?? 'media'}
				class="max-h-[60vh] max-w-full rounded object-contain"
			/>
		{/if}
	</div>
{/if}
