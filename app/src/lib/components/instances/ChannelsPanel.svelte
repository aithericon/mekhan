<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import Radio from '@lucide/svelte/icons/radio';
	import Play from '@lucide/svelte/icons/play';
	import Square from '@lucide/svelte/icons/square';
	import AudioLines from '@lucide/svelte/icons/audio-lines';
	import { authFetch } from '$lib/auth/fetch';
	import { isRawPcm, parsePcmParams, pcmToWavBlob } from '$lib/audio/pcmWav';
	import { playLivePcm, parseSampleRate } from '$lib/audio/livePcmPlayer';
	import { planLiveRender, planFileRender, type LiveRenderPlan } from '$lib/channels/renderers';
	import { playMseStream } from '$lib/channels/mseStreamPlayer';
	import { playMjpegStream } from '$lib/channels/mjpegStreamPlayer';
	import { subscribe as subscribeLiveTap } from '$lib/channels/liveTapRegistry';
	import MediaPlayer from './output-renderers/MediaPlayer.svelte';
	import type { Channel } from '$lib/api/client';
	import type { ChannelRuntime } from '$lib/stores/instance-marking.svelte';

	type Props = {
		/** The node's statically-declared channels (docs/25). */
		channels: Channel[];
		/** Per-channel live lifecycle, keyed by channel name. Absent → no live
		 *  marking available; channels render statically. */
		runtime?: Record<string, ChannelRuntime> | null;
		/** Runtime execution id (from the step's result envelope) — the key the
		 *  tap endpoint scopes channel bytes by. Without it the Play affordance is
		 *  hidden (we can't address the stream). */
		executionId?: string | null;
	};

	let { channels, runtime = null, executionId = null }: Props = $props();

	// `element.content_type` lives only on the binary arm; surface it generically.
	function elementContentType(ch: Channel): string | null {
		const el = ch.element as { type: string; content_type?: string };
		return el.type === 'binary' ? (el.content_type ?? null) : null;
	}

	function elementLabel(ch: Channel): string {
		const el = ch.element as { type: string; content_type?: string };
		if (el.type === 'binary') return `binary · ${el.content_type ?? 'octet-stream'}`;
		return el.type; // 'json' | 'any'
	}

	// All playback is for OUT data channels — the producer side carries bytes.
	function isOutData(ch: Channel): boolean {
		return ch.direction === 'out' && ch.plane === 'data';
	}

	// Dispatch through the render-adapter registry (the presentation-side analog
	// of the wire transport dispatch): the element content_type selects the
	// renderer, so the panel never re-derives "is this playable?" ad hoc.

	// A channel is whole-file previewable when it's an OUT data channel whose
	// binary element content_type is audio/video/image (PCM included — WAV-wrapped
	// client-side below).
	function isPlayable(ch: Channel): boolean {
		return isOutData(ch) && planFileRender(elementContentType(ch)) !== null;
	}

	// The LIVE render plan for a channel (pcm → Web Audio, mse → MediaSource), or
	// null when there's no live path. Computed off the element content_type; the
	// per-envelope tap content-type (which may add runtime params like a sample
	// rate) refines it at play time.
	function livePlan(ch: Channel): LiveRenderPlan | null {
		if (!isOutData(ch)) return null;
		return planLiveRender(elementContentType(ch));
	}

	// Per-channel media-preview state (lazy, fetched on click). Keyed by name.
	type Preview = {
		loading: boolean;
		error: string | null;
		// What MediaPlayer renders: a resolved object-URL ref.
		ref: { url: string; content_type: string; filename: string } | null;
	};
	let previews = $state<Record<string, Preview>>({});
	// objectURLs to revoke on teardown.
	let objectUrls: string[] = [];

	async function play(ch: Channel) {
		if (!executionId) return;
		// Revoke a prior preview for this channel before replacing it, so a
		// re-tap doesn't orphan the old blob URL for the panel's lifetime.
		const prevUrl = previews[ch.name]?.ref?.url ?? null;
		if (prevUrl) {
			URL.revokeObjectURL(prevUrl);
			objectUrls = objectUrls.filter((u) => u !== prevUrl);
		}
		previews[ch.name] = { loading: true, error: null, ref: null };
		try {
			const r = await authFetch(
				`/api/v1/executions/${executionId}/channels/${encodeURIComponent(ch.name)}/data`
			);
			if (!r.ok) throw new Error(`tap fetch failed: ${r.status}`);
			// Prefer the tap response's Content-Type — the per-envelope value
			// carries runtime params (e.g. `audio/L16;rate=16000`) the static
			// channel declaration lacks. Fall back to the declared element type.
			const ct =
				r.headers.get('content-type') ?? elementContentType(ch) ?? 'application/octet-stream';
			const raw = new Uint8Array(await r.arrayBuffer());

			let blob: Blob;
			let playCt: string;
			if (isRawPcm(ct)) {
				// <audio> can't decode headerless PCM — wrap into a WAV container.
				blob = pcmToWavBlob(raw, parsePcmParams(ct));
				playCt = 'audio/wav';
			} else {
				// Already-playable (audio/wav, audio/mpeg, video/*, image/*).
				blob = new Blob([raw], { type: ct });
				playCt = ct;
			}
			const url = URL.createObjectURL(blob);
			objectUrls.push(url);
			previews[ch.name] = {
				loading: false,
				error: null,
				ref: { url, content_type: playCt, filename: ch.name }
			};
		} catch (e) {
			previews[ch.name] = {
				loading: false,
				error: e instanceof Error ? e.message : String(e),
				ref: null
			};
		}
	}

	// ── Live playback (Web Audio for PCM, MSE for fragmented audio/video) ──────
	// Both live renderers share a `stop()`-able handle + the same status union,
	// so one state map drives them uniformly.
	type LiveStatus = 'streaming' | 'ended' | 'stopped' | 'error';
	type Live = {
		status: LiveStatus;
		seconds: number;
		bytes: number;
		error: string | null;
		handle: { stop(): void } | null;
		// The shared-tap subscription's ref-count release (last release cancels the
		// single underlying source read). Decoupled from `handle` (the player's
		// stop) because both must run on teardown.
		release: (() => void) | null;
	};
	let lives = $state<Record<string, Live>>({});
	// MSE renders into a real media element; bound per channel below.
	let mediaEls = $state<Record<string, HTMLMediaElement | null>>({});
	// MJPEG swaps each decoded frame into an <img>; bound per channel below.
	let imgEls = $state<Record<string, HTMLImageElement | null>>({});

	function startLive(ch: Channel, status: LiveStatus = 'streaming') {
		lives[ch.name] = { status, seconds: 0, bytes: 0, error: null, handle: null, release: null };
	}

	function onLiveStatus(ch: Channel) {
		return (status: LiveStatus, error?: string) => {
			const cur = lives[ch.name];
			if (cur) lives[ch.name] = { ...cur, status, error: error ?? cur.error };
		};
	}

	function onLiveProgress(ch: Channel) {
		return (seconds: number, bytes: number) => {
			const cur = lives[ch.name];
			if (cur) lives[ch.name] = { ...cur, seconds, bytes };
		};
	}

	// Open the shared live tap (`?follow=1`) via the ref-counting registry, which
	// opens ONE source read per (execution, channel) and fans it out. Returns this
	// subscriber's private stream + a `release()` (last release cancels the source).
	// The registry now owns the tap Response, so the per-envelope content-type
	// refinement is no longer read here; we use the static element content_type
	// decl as the mime (same value `livePlan`/`plan.mime` already derive from), and
	// PCM's `parseSampleRate` falls back correctly when the decl lacks a rate param.
	function openLiveTap(ch: Channel, kind: LiveRenderPlan['kind']) {
		return subscribeLiveTap(executionId!, ch.name, kind);
	}

	// PCM → schedule on a Web Audio timeline.
	function playLivePcmChannel(ch: Channel) {
		if (!executionId) return;
		stopLive(ch);
		startLive(ch);
		try {
			const sub = openLiveTap(ch, 'pcm');
			const handle = playLivePcm({
				stream: sub.stream,
				sampleRate: parseSampleRate(elementContentType(ch)),
				onStatus: onLiveStatus(ch),
				onProgress: onLiveProgress(ch)
			});
			const cur = lives[ch.name];
			if (cur) lives[ch.name] = { ...cur, handle, release: sub.release };
		} catch (e) {
			lives[ch.name] = {
				status: 'error',
				seconds: 0,
				bytes: 0,
				error: e instanceof Error ? e.message : String(e),
				handle: null,
				release: null
			};
		}
	}

	// Fragmented audio/video → append into a MediaSource on the bound element.
	function playLiveMse(ch: Channel, plan: LiveRenderPlan) {
		if (!executionId) return;
		const media = mediaEls[ch.name];
		if (!media) return;
		stopLive(ch);
		startLive(ch);
		try {
			const sub = openLiveTap(ch, 'mse');
			const handle = playMseStream({
				stream: sub.stream,
				mimeType: plan.mime,
				media,
				onStatus: onLiveStatus(ch),
				onProgress: onLiveProgress(ch)
			});
			const cur = lives[ch.name];
			if (cur) lives[ch.name] = { ...cur, handle, release: sub.release };
		} catch (e) {
			lives[ch.name] = {
				status: 'error',
				seconds: 0,
				bytes: 0,
				error: e instanceof Error ? e.message : String(e),
				handle: null,
				release: null
			};
		}
	}

	// Motion-JPEG → swap each decoded frame into the bound <img>. `onProgress`
	// reports (framesRendered, bytes); the shared `seconds` slot carries the frame
	// count (labelled "frames" for mjpeg below).
	function playLiveMjpeg(ch: Channel, plan: LiveRenderPlan) {
		if (!executionId) return;
		const img = imgEls[ch.name];
		if (!img) return;
		stopLive(ch);
		startLive(ch);
		try {
			const sub = openLiveTap(ch, 'mjpeg');
			const handle = playMjpegStream({
				stream: sub.stream,
				img,
				mime: plan.mime,
				onStatus: onLiveStatus(ch),
				onProgress: onLiveProgress(ch)
			});
			const cur = lives[ch.name];
			if (cur) lives[ch.name] = { ...cur, handle, release: sub.release };
		} catch (e) {
			lives[ch.name] = {
				status: 'error',
				seconds: 0,
				bytes: 0,
				error: e instanceof Error ? e.message : String(e),
				handle: null,
				release: null
			};
		}
	}

	function stopLive(ch: Channel) {
		const cur = lives[ch.name];
		cur?.handle?.stop();
		cur?.release?.();
	}

	$effect(() => {
		return () => {
			for (const u of objectUrls) URL.revokeObjectURL(u);
			objectUrls = [];
			for (const l of Object.values(lives)) {
				l.handle?.stop();
				l.release?.();
			}
		};
	});

	function statusLabel(rt: ChannelRuntime | undefined): string | null {
		if (!rt || !rt.known) return null;
		const parts: string[] = [];
		parts.push(rt.opened ? 'opened' : 'idle');
		parts.push(`${rt.elements} element${rt.elements === 1 ? '' : 's'}`);
		if (rt.closed) parts.push('closed');
		return parts.join(' · ');
	}

	// Live activity is "in progress" for the Play/Stop toggle.
	function isLiveActive(live: Live | undefined): boolean {
		return !!live && (live.status === 'streaming' || live.status === 'ended');
	}
</script>

<section data-testid="channels-panel">
	<h3 class="mb-2 flex items-center gap-2 text-sm font-semibold text-foreground">
		<Radio class="size-4 text-muted-foreground" />
		Channels
		<Badge variant="outline" class="font-mono text-sm font-normal">{channels.length}</Badge>
	</h3>
	<div class="divide-y divide-border rounded-md border border-border">
		{#each channels as ch (ch.name)}
			{@const rt = runtime?.[ch.name]}
			{@const status = statusLabel(rt)}
			{@const preview = previews[ch.name]}
			{@const live = lives[ch.name]}
			{@const lplan = livePlan(ch)}
			<div class="px-3 py-2 text-sm">
				<div class="flex flex-wrap items-center gap-1.5">
					<span class="font-mono font-medium text-foreground break-all">{ch.name}</span>
					<Badge variant="outline" class="font-mono text-sm font-normal">{ch.direction}</Badge>
					<Badge variant="outline" class="font-mono text-sm font-normal">{ch.plane}</Badge>
					<Badge variant="outline" class="font-mono text-sm font-normal">{elementLabel(ch)}</Badge>
					{#if status}
						<span class="ml-auto font-mono text-sm text-muted-foreground">{status}</span>
					{/if}
				</div>

				{#if isPlayable(ch)}
					<div class="mt-2">
						{#if !preview || (!preview.loading && !preview.ref && !preview.error)}
							<Button
								variant="outline"
								size="sm"
								disabled={!executionId}
								onclick={() => play(ch)}
								title={executionId
									? 'Fetch and preview the channel payload'
									: 'Execution id unavailable — cannot tap this channel'}
							>
								<Play class="size-4" />
								<span class="ml-1.5">Play / Preview</span>
							</Button>
						{:else if preview.loading}
							<div class="text-sm text-muted-foreground">Fetching channel payload…</div>
						{:else if preview.error}
							<div class="text-sm text-red-500">{preview.error}</div>
						{:else if preview.ref}
							<MediaPlayer value={preview.ref} ctx={{ position: 'output' }} />
						{/if}
					</div>
				{/if}

				{#if lplan}
					<div class="mt-2 flex flex-wrap items-center gap-2">
						{#if !isLiveActive(live)}
							<Button
								variant="outline"
								size="sm"
								disabled={!executionId}
								onclick={() =>
									lplan.kind === 'pcm'
										? playLivePcmChannel(ch)
										: lplan.kind === 'mjpeg'
											? playLiveMjpeg(ch, lplan)
											: playLiveMse(ch, lplan)}
								title={executionId
									? `Stream and play this channel live (${
											lplan.kind === 'pcm'
												? 'Web Audio'
												: lplan.kind === 'mjpeg'
													? 'MJPEG'
													: 'Media Source'
										}) while the step produces`
									: 'Execution id unavailable — cannot tap this channel'}
							>
								<AudioLines class="size-4" />
								<span class="ml-1.5">Play live</span>
							</Button>
						{:else}
							<Button variant="outline" size="sm" onclick={() => stopLive(ch)}>
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
								· {lplan.kind === 'mjpeg'
									? `${live.seconds} frame${live.seconds === 1 ? '' : 's'}`
									: `${live.seconds.toFixed(1)}s`} · {(live.bytes / 1024).toFixed(0)} KB
							</span>
						{:else if live && live.status === 'error'}
							<span class="text-sm text-red-500">{live.error}</span>
						{/if}
					</div>

					{#if lplan.kind === 'mse'}
						<!-- MSE renders into a real element; kept mounted so the
						     MediaSource can attach the moment Play live is clicked. -->
						<div class="mt-2" class:hidden={!isLiveActive(live)}>
							{#if lplan.mediaKind === 'video'}
								<!-- svelte-ignore a11y_media_has_caption -->
								<video
									bind:this={mediaEls[ch.name]}
									controls
									class="max-h-64 w-full rounded-md bg-black"
								></video>
							{:else}
								<audio bind:this={mediaEls[ch.name]} controls class="w-full"></audio>
							{/if}
						</div>
					{:else if lplan.kind === 'mjpeg'}
						<!-- MJPEG swaps each decoded JPEG frame into this <img>; kept
						     mounted so the player can target it the moment Play live runs. -->
						<div class="mt-2" class:hidden={!isLiveActive(live)}>
							<!-- svelte-ignore a11y_img_redundant_alt -->
							<img
								bind:this={imgEls[ch.name]}
								alt="live frame"
								class="max-h-64 w-full rounded-md bg-black object-contain"
							/>
						</div>
					{/if}
				{/if}
			</div>
		{/each}
	</div>
</section>
