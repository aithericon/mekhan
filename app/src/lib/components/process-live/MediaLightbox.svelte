<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import X from '@lucide/svelte/icons/x';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import Download from '@lucide/svelte/icons/download';
	import Film from '@lucide/svelte/icons/film';
	import AudioLines from '@lucide/svelte/icons/audio-lines';
	import ChartLine from '@lucide/svelte/icons/chart-line';
	import { catalogueDownloadUrl, type LiveArtifactEntry } from '$lib/api/client';
	import { authFetch } from '$lib/auth/fetch';
	import { pickRenderer } from './renderers/registry';

	// Full-screen lightbox over one artifact group: big media stage, prev/next
	// arrows, thumbnail gallery strip, keyboard navigation. Controlled by the
	// host (ArtifactsPanel) so the panel's scrubber position and the lightbox
	// selection stay one piece of state.
	interface Props {
		entries: LiveArtifactEntry[];
		index: number;
		label?: string;
		onnavigate: (i: number) => void;
		onclose: () => void;
	}
	let { entries, index, label, onnavigate, onclose }: Props = $props();

	const clamped = $derived(Math.min(Math.max(index, 0), entries.length - 1));
	const entry = $derived(entries[clamped]);

	type Kind = 'image' | 'video' | 'audio' | 'renderer';
	function kindOf(e: LiveArtifactEntry): Kind {
		const mime = e.mime_type ?? '';
		if (mime.startsWith('image/')) return 'image';
		if (mime.startsWith('video/')) return 'video';
		if (mime.startsWith('audio/')) return 'audio';
		return 'renderer';
	}
	const kind = $derived(kindOf(entry));
	const Renderer = $derived(pickRenderer(entry));

	// ── auth'd blob cache ───────────────────────────────────────────────────
	// <img>/<video>/<audio> can't carry an Authorization header, so media bytes
	// are fetched via authFetch and served as object URLs (same approach as the
	// inline renderers). One cache serves both the stage and the thumbnails so
	// nothing is downloaded twice; everything is revoked when the lightbox
	// unmounts.
	let srcByPath = $state<Record<string, string>>({});
	let errorByPath = $state<Record<string, string>>({});
	const pending = new Set<string>();

	function requestSrc(path: string | null) {
		if (!path || srcByPath[path] || errorByPath[path] || pending.has(path)) return;
		pending.add(path);
		authFetch(catalogueDownloadUrl(path))
			.then((r) => {
				if (!r.ok) throw new Error(`fetch failed: ${r.status}`);
				return r.blob();
			})
			.then((b) => {
				srcByPath[path] = URL.createObjectURL(b);
			})
			.catch((e) => {
				errorByPath[path] = e instanceof Error ? e.message : String(e);
			})
			.finally(() => pending.delete(path));
	}

	$effect(() => {
		return () => {
			for (const url of Object.values(srcByPath)) URL.revokeObjectURL(url);
		};
	});

	// Fetch the focused entry's bytes plus all image thumbnails (videos/audio
	// get icon tiles, so only the focused one ever downloads).
	$effect(() => {
		if (kind !== 'renderer') requestSrc(entry.storage_path);
		for (const e of entries) {
			if (kindOf(e) === 'image') requestSrc(e.storage_path);
		}
	});

	const stageSrc = $derived(entry.storage_path ? (srcByPath[entry.storage_path] ?? null) : null);
	const stageError = $derived(
		entry.storage_path ? (errorByPath[entry.storage_path] ?? null) : 'no storage_path'
	);

	function go(i: number) {
		if (i < 0 || i > entries.length - 1) return;
		onnavigate(i);
	}

	function onkeydown(ev: KeyboardEvent) {
		if (ev.key === 'Escape') {
			ev.preventDefault();
			onclose();
		} else if (ev.key === 'ArrowLeft') {
			ev.preventDefault();
			go(clamped - 1);
		} else if (ev.key === 'ArrowRight') {
			ev.preventDefault();
			go(clamped + 1);
		}
	}

	// Lock background scroll while the overlay is up.
	$effect(() => {
		const prev = document.body.style.overflow;
		document.body.style.overflow = 'hidden';
		return () => {
			document.body.style.overflow = prev;
		};
	});

	// Keep the selected thumbnail in view as the user navigates.
	let thumbStrip = $state<HTMLElement | null>(null);
	$effect(() => {
		void clamped;
		thumbStrip
			?.querySelector('[data-selected="true"]')
			?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
	});

	function formatBytes(b: number | null | undefined): string | null {
		if (b === undefined || b === null) return null;
		if (b < 1024) return `${b} B`;
		if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
		return `${(b / (1024 * 1024)).toFixed(1)} MB`;
	}
</script>

<svelte:window {onkeydown} />

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
	class="fixed inset-0 z-50 flex flex-col bg-black/90 backdrop-blur-sm"
	role="dialog"
	aria-modal="true"
	aria-label={label ? `${label} viewer` : 'Artifact viewer'}
>
	<!-- Header: identity left, actions right -->
	<div class="flex shrink-0 items-center justify-between gap-2 px-4 py-3">
		<div class="flex min-w-0 items-center gap-2">
			{#if label}
				<Badge variant="secondary" class="font-mono text-sm">{label}</Badge>
			{/if}
			<span class="truncate text-sm text-white/90">{entry.name}</span>
			{#if entry.process_step}
				<Badge variant="outline" class="border-white/20 text-sm text-white/70">
					step {entry.process_step}
				</Badge>
			{/if}
			{#if formatBytes(entry.size_bytes)}
				<span class="text-sm text-white/50">{formatBytes(entry.size_bytes)}</span>
			{/if}
		</div>
		<div class="flex shrink-0 items-center gap-1">
			<span class="mr-2 text-sm tabular-nums text-white/60">
				{clamped + 1} / {entries.length}
			</span>
			{#if entry.storage_path}
				<a
					href={catalogueDownloadUrl(entry.storage_path)}
					download={entry.name || entry.filename}
					class="inline-flex size-8 items-center justify-center rounded-md text-white/70 transition-colors hover:bg-white/10 hover:text-white"
					title="Download"
				>
					<Download class="size-4" />
				</a>
			{/if}
			<button
				type="button"
				class="inline-flex size-8 items-center justify-center rounded-md text-white/70 transition-colors hover:bg-white/10 hover:text-white"
				title="Close (Esc)"
				onclick={onclose}
			>
				<X class="size-5" />
			</button>
		</div>
	</div>

	<!-- Stage: click on the backdrop closes; arrows overlaid left/right -->
	<div class="relative flex min-h-0 flex-1 items-center justify-center px-14 pb-2">
		<button
			type="button"
			class="absolute inset-0"
			aria-label="Close"
			tabindex="-1"
			onclick={onclose}
		></button>

		{#if kind === 'image'}
			{#if stageSrc}
				<img
					src={stageSrc}
					alt={entry.name}
					class="pointer-events-none relative max-h-full max-w-full rounded-lg object-contain"
				/>
			{:else if stageError}
				<p class="relative text-sm text-red-400">Failed to load: {stageError}</p>
			{:else}
				<p class="relative text-sm text-white/60">Loading…</p>
			{/if}
		{:else if kind === 'video'}
			{#if stageSrc}
				<!-- svelte-ignore a11y_media_has_caption -->
				<video controls autoplay src={stageSrc} class="relative max-h-full max-w-full rounded-lg">
					Your browser does not support the video element.
				</video>
			{:else if stageError}
				<p class="relative text-sm text-red-400">Failed to load: {stageError}</p>
			{:else}
				<p class="relative text-sm text-white/60">Loading…</p>
			{/if}
		{:else if kind === 'audio'}
			{#if stageSrc}
				<div
					class="relative flex w-full max-w-xl flex-col items-center gap-4 rounded-xl bg-white/5 p-8"
				>
					<AudioLines class="size-10 text-white/60" />
					<audio controls src={stageSrc} class="w-full">
						Your browser does not support the audio element.
					</audio>
				</div>
			{:else if stageError}
				<p class="relative text-sm text-red-400">Failed to load: {stageError}</p>
			{:else}
				<p class="relative text-sm text-white/60">Loading…</p>
			{/if}
		{:else if Renderer}
			<!-- Hint renderers (e.g. gp-posterior) carry their own chrome; give
			     them a card surface so charts stay legible on the dark backdrop. -->
			<div
				class="relative max-h-full w-full max-w-5xl overflow-auto rounded-xl border border-border bg-background p-4"
			>
				<Renderer {entry} />
			</div>
		{:else}
			<p class="relative text-sm text-white/60">No renderer for this artifact.</p>
		{/if}

		{#if entries.length > 1}
			<button
				type="button"
				class="absolute left-2 top-1/2 -translate-y-1/2 rounded-full bg-white/10 p-2 text-white/80 transition-colors hover:bg-white/20 hover:text-white disabled:pointer-events-none disabled:opacity-30"
				disabled={clamped === 0}
				title="Previous (←)"
				onclick={() => go(clamped - 1)}
			>
				<ChevronLeft class="size-6" />
			</button>
			<button
				type="button"
				class="absolute right-2 top-1/2 -translate-y-1/2 rounded-full bg-white/10 p-2 text-white/80 transition-colors hover:bg-white/20 hover:text-white disabled:pointer-events-none disabled:opacity-30"
				disabled={clamped === entries.length - 1}
				title="Next (→)"
				onclick={() => go(clamped + 1)}
			>
				<ChevronRight class="size-6" />
			</button>
		{/if}
	</div>

	<!-- Thumbnail gallery -->
	{#if entries.length > 1}
		<div
			bind:this={thumbStrip}
			class="flex shrink-0 items-center gap-2 overflow-x-auto px-4 py-3"
		>
			{#each entries as e, i (e.artifact_id ?? e.id ?? i)}
				{@const k = kindOf(e)}
				{@const thumb = e.storage_path ? srcByPath[e.storage_path] : null}
				<button
					type="button"
					data-selected={i === clamped}
					class="relative size-16 shrink-0 overflow-hidden rounded-md border transition-all {i ===
					clamped
						? 'border-primary ring-2 ring-primary'
						: 'border-white/15 opacity-60 hover:opacity-100'}"
					title={e.process_step ? `${e.name} · step ${e.process_step}` : e.name}
					onclick={() => go(i)}
				>
					{#if k === 'image' && thumb}
						<img src={thumb} alt={e.name} class="size-full object-cover" loading="lazy" />
					{:else}
						<span class="flex size-full items-center justify-center bg-white/5 text-white/60">
							{#if k === 'video'}
								<Film class="size-5" />
							{:else if k === 'audio'}
								<AudioLines class="size-5" />
							{:else}
								<ChartLine class="size-5" />
							{/if}
						</span>
					{/if}
					{#if e.process_step}
						<span
							class="absolute bottom-0 right-0 rounded-tl bg-black/70 px-1 text-[10px] tabular-nums text-white/80"
						>
							{e.process_step}
						</span>
					{/if}
				</button>
			{/each}
		</div>
	{/if}
</div>
