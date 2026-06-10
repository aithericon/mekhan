<script lang="ts">
	// Instance-view panel for a `stream_sink` node: surfaces the node's STABLE
	// egress URL (external consumers tap the sink's bytes there — the whole
	// point of the node) with a copy affordance, plus an in-panel live preview
	// when the sink channel's content_type has a live renderer
	// (`planLiveRender`: pcm → Web Audio, mse → MediaSource, mjpeg → <img>).
	//
	// The preview reuses the existing live players directly on a `?follow=1`
	// fetch of the egress URL. It deliberately does NOT go through the
	// execution-keyed `liveTapRegistry` — that registry's tap URL is
	// `/api/v1/executions/{id}/channels/{name}/data`, while a sink's bytes live
	// at the instance-scoped egress route below.
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import CopyButton from '$lib/components/ui/copy-button/CopyButton.svelte';
	import SatelliteDish from '@lucide/svelte/icons/satellite-dish';
	import AudioLines from '@lucide/svelte/icons/audio-lines';
	import Square from '@lucide/svelte/icons/square';
	import { authFetch } from '$lib/auth/fetch';
	import { planLiveRender, type LiveRenderPlan } from '$lib/channels/renderers';
	import { playLivePcm, parseSampleRate } from '$lib/audio/livePcmPlayer';
	import { playMseStream } from '$lib/channels/mseStreamPlayer';
	import { playMjpegStream } from '$lib/channels/mjpegStreamPlayer';
	import { playTextStream, tailCap } from '$lib/channels/textStreamPlayer';
	import type { Channel } from '$lib/api/client';
	import type { ChannelRuntime } from '$lib/stores/instance-marking.svelte';

	type Props = {
		/** Owning workflow instance id (the egress URL is instance-scoped). */
		instanceId: string;
		/** The stream_sink node id (the egress URL's second key). */
		nodeId: string;
		/** The sink's declared channels — v1 carries exactly one IN entry; the
		 *  first one's content_type drives the live-preview plan. */
		channels: Channel[];
		/** Per-channel live lifecycle keyed by channel name (best-effort). */
		runtime?: Record<string, ChannelRuntime> | null;
	};

	let { instanceId, nodeId, channels, runtime = null }: Props = $props();

	/** The stable egress URL external consumers tap. */
	const egressPath = $derived(`/api/v1/instances/${instanceId}/sinks/${nodeId}/data`);
	// Absolute URL is what an EXTERNAL consumer needs — prefer it for copy/display
	// when a browser origin is available (SSR-safe fallback to the path).
	const egressUrl = $derived(
		typeof window !== 'undefined' ? `${window.location.origin}${egressPath}` : egressPath
	);

	const channel = $derived<Channel | null>(channels[0] ?? null);
	const contentType = $derived.by<string | null>(() => {
		const el = channel?.element as { type: string; content_type?: string } | undefined;
		return el?.type === 'binary' ? (el.content_type ?? null) : null;
	});
	const plan = $derived<LiveRenderPlan | null>(planLiveRender(contentType));
	const rt = $derived(channel ? runtime?.[channel.name] : undefined);

	const statusLabel = $derived.by<string | null>(() => {
		if (!rt || !rt.known) return null;
		const parts: string[] = [];
		parts.push(rt.opened ? 'opened' : 'idle');
		parts.push(`${rt.elements} element${rt.elements === 1 ? '' : 's'}`);
		if (rt.closed) parts.push('closed');
		return parts.join(' · ');
	});

	// ── Live preview state (mirrors ChannelsPanel's single-channel machinery) ──
	type LiveStatus = 'streaming' | 'ended' | 'stopped' | 'error';
	let live = $state<{
		status: LiveStatus;
		seconds: number;
		bytes: number;
		error: string | null;
		handle: { stop(): void } | null;
		abort: AbortController | null;
	} | null>(null);
	let mediaEl = $state<HTMLMediaElement | null>(null);
	let imgEl = $state<HTMLImageElement | null>(null);
	let textPreEl = $state<HTMLPreElement | null>(null);
	// Accumulated tail of a live text preview (capped — a long-running feed
	// must not grow panel state without bound).
	const TEXT_TAIL_CAP = 20_000;
	let liveText = $state('');

	const isLiveActive = $derived(!!live && (live.status === 'streaming' || live.status === 'ended'));

	function onStatus(status: LiveStatus, error?: string) {
		if (live) live = { ...live, status, error: error ?? live.error };
	}
	function onProgress(seconds: number, bytes: number) {
		if (live) live = { ...live, seconds, bytes };
	}

	async function playLive() {
		if (!plan) return;
		stopLive();
		const abort = new AbortController();
		live = { status: 'streaming', seconds: 0, bytes: 0, error: null, handle: null, abort };
		try {
			const r = await authFetch(`${egressPath}?follow=1`, { signal: abort.signal });
			if (!r.ok) throw new Error(`egress tap failed: ${r.status}`);
			if (!r.body) throw new Error('egress tap returned no body');
			// Prefer the response's content-type (it may carry runtime params like
			// a PCM sample rate) over the static channel declaration.
			const ct = r.headers.get('content-type') ?? contentType ?? 'application/octet-stream';
			let handle: { stop(): void };
			if (plan.kind === 'pcm') {
				handle = playLivePcm({
					stream: r.body,
					sampleRate: parseSampleRate(ct),
					onStatus,
					onProgress
				});
			} else if (plan.kind === 'mjpeg') {
				if (!imgEl) throw new Error('no image element bound');
				handle = playMjpegStream({ stream: r.body, img: imgEl, mime: plan.mime, onStatus, onProgress });
			} else if (plan.kind === 'mse') {
				if (!mediaEl) throw new Error('no media element bound');
				handle = playMseStream({
					stream: r.body,
					mimeType: plan.mime,
					media: mediaEl,
					onStatus,
					onProgress
				});
			} else if (plan.kind === 'text') {
				liveText = '';
				handle = playTextStream({
					stream: r.body,
					onText: (t) => (liveText = tailCap(liveText + t, TEXT_TAIL_CAP)),
					onStatus,
					onProgress
				});
			} else {
				// urdf / scene twins are graph-edge renderers; no in-drawer preview.
				throw new Error(`no in-panel preview for '${plan.kind}'`);
			}
			if (live) live = { ...live, handle };
		} catch (e) {
			live = {
				status: 'error',
				seconds: 0,
				bytes: 0,
				error: e instanceof Error ? e.message : String(e),
				handle: null,
				abort: null
			};
		}
	}

	function stopLive() {
		live?.handle?.stop();
		live?.abort?.abort();
		if (live) live = { ...live, status: 'stopped' };
	}

	$effect(() => {
		return () => {
			live?.handle?.stop();
			live?.abort?.abort();
		};
	});

	// Pin the text preview to its tail as new text lands.
	$effect(() => {
		void liveText;
		if (textPreEl) textPreEl.scrollTop = textPreEl.scrollHeight;
	});
</script>

<section data-testid="stream-sink-panel">
	<h3 class="mb-2 flex items-center gap-2 text-sm font-semibold text-foreground">
		<SatelliteDish class="size-4 text-muted-foreground" />
		Egress endpoint
		{#if channel}
			<Badge variant="outline" class="font-mono text-sm font-normal">{channel.name}</Badge>
		{/if}
		{#if statusLabel}
			<span class="ml-auto font-mono text-sm font-normal text-muted-foreground">{statusLabel}</span>
		{/if}
	</h3>

	<div class="rounded-md border border-border px-3 py-2 text-sm">
		<p class="mb-1.5 text-muted-foreground">
			External consumers can tap this node's stream at the stable URL:
		</p>
		<div class="flex items-center gap-1.5">
			<code
				class="min-w-0 flex-1 truncate rounded bg-muted/40 px-2 py-1 font-mono text-foreground"
				title={egressUrl}
				data-testid="sink-egress-url">{egressUrl}</code>
			<CopyButton text={egressUrl} title="Copy egress URL" />
		</div>
		{#if contentType}
			<div class="mt-1.5 font-mono text-sm text-muted-foreground">{contentType}</div>
		{/if}

		{#if plan && (plan.kind === 'pcm' || plan.kind === 'mse' || plan.kind === 'mjpeg' || plan.kind === 'text')}
			<div class="mt-2 flex flex-wrap items-center gap-2">
				{#if !isLiveActive}
					<Button
						variant="outline"
						size="sm"
						onclick={playLive}
						title={`Stream and play the sink's bytes live (${
							plan.kind === 'pcm'
								? 'Web Audio'
								: plan.kind === 'mjpeg'
									? 'MJPEG'
									: plan.kind === 'text'
										? 'text console'
										: 'Media Source'
						})`}
					>
						<AudioLines class="size-4" />
						<span class="ml-1.5">Play live</span>
					</Button>
				{:else}
					<Button variant="outline" size="sm" onclick={stopLive}>
						<Square class="size-4" />
						<span class="ml-1.5">Stop</span>
					</Button>
				{/if}
				{#if live && (live.status === 'streaming' || live.status === 'ended')}
					<span
						class="font-mono text-sm text-muted-foreground"
						class:text-foreground={live.status === 'streaming'}
					>
						{#if live.status === 'streaming'}<span class="text-red-500">●</span> live{:else}ended{/if}
						· {plan.kind === 'mjpeg'
							? `${live.seconds} frame${live.seconds === 1 ? '' : 's'}`
							: plan.kind === 'text'
								? `${live.seconds} char${live.seconds === 1 ? '' : 's'}`
								: `${live.seconds.toFixed(1)}s`} · {(live.bytes / 1024).toFixed(0)} KB
					</span>
				{:else if live && live.status === 'error'}
					<span class="text-sm text-red-500">{live.error}</span>
				{/if}
			</div>

			{#if plan.kind === 'mse'}
				<!-- MSE renders into a real element; kept mounted so the MediaSource
				     can attach the moment Play live runs. -->
				<div class="mt-2" class:hidden={!isLiveActive}>
					{#if plan.mediaKind === 'video'}
						<!-- svelte-ignore a11y_media_has_caption -->
						<video bind:this={mediaEl} controls class="max-h-64 w-full rounded-md bg-black"></video>
					{:else}
						<audio bind:this={mediaEl} controls class="w-full"></audio>
					{/if}
				</div>
			{:else if plan.kind === 'mjpeg'}
				<div class="mt-2" class:hidden={!isLiveActive}>
					<!-- svelte-ignore a11y_img_redundant_alt -->
					<img
						bind:this={imgEl}
						alt="live frame"
						class="max-h-64 w-full rounded-md bg-black object-contain"
					/>
				</div>
			{:else if plan.kind === 'text'}
				<!-- Live text console: decoded UTF-8 appends here, pinned to the tail. -->
				<div class="mt-2" class:hidden={!isLiveActive}>
					<pre
						bind:this={textPreEl}
						data-testid="sink-text-preview"
						class="max-h-64 w-full overflow-y-auto rounded-md bg-black/90 px-3 py-2 font-mono text-xs whitespace-pre-wrap break-words text-emerald-100">{liveText}</pre>
				</div>
			{/if}
		{/if}
	</div>
</section>
