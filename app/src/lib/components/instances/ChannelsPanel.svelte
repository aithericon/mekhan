<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import Radio from '@lucide/svelte/icons/radio';
	import Play from '@lucide/svelte/icons/play';
	import Square from '@lucide/svelte/icons/square';
	import AudioLines from '@lucide/svelte/icons/audio-lines';
	import { authFetch } from '$lib/auth/fetch';
	import { isRawPcm, parsePcmParams, pcmToWavBlob } from '$lib/audio/pcmWav';
	import {
		playLivePcm,
		parseSampleRate,
		type LivePcmHandle,
		type LivePcmStatus
	} from '$lib/audio/livePcmPlayer';
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

	// A channel is playable when it's an OUT data channel carrying a binary
	// element whose content_type is audio/video/image (PCM included — we wrap it
	// client-side below).
	function isPlayable(ch: Channel): boolean {
		const ct = elementContentType(ch);
		return (
			ch.direction === 'out' &&
			ch.plane === 'data' &&
			ct !== null &&
			/^(audio|video|image)\//.test(ct)
		);
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

	// A raw-PCM audio channel can be played LIVE through Web Audio while the
	// producing step is still running (the tap's `?follow=1` streams as it lands).
	function isLivePcm(ch: Channel): boolean {
		return (
			ch.direction === 'out' &&
			ch.plane === 'data' &&
			isRawPcm(elementContentType(ch) ?? '')
		);
	}

	type Live = {
		status: LivePcmStatus;
		seconds: number;
		bytes: number;
		error: string | null;
		handle: LivePcmHandle | null;
	};
	let lives = $state<Record<string, Live>>({});

	async function playLive(ch: Channel) {
		if (!executionId) return;
		stopLive(ch);
		lives[ch.name] = { status: 'streaming', seconds: 0, bytes: 0, error: null, handle: null };
		try {
			const r = await authFetch(
				`/api/v1/executions/${executionId}/channels/${encodeURIComponent(ch.name)}/data?follow=1`
			);
			if (!r.ok || !r.body) throw new Error(`live tap failed: ${r.status}`);
			const ct = r.headers.get('content-type') ?? elementContentType(ch);
			const handle = playLivePcm({
				stream: r.body,
				sampleRate: parseSampleRate(ct),
				onStatus: (status, error) => {
					const cur = lives[ch.name];
					if (cur) lives[ch.name] = { ...cur, status, error: error ?? cur.error };
				},
				onProgress: (seconds, bytes) => {
					const cur = lives[ch.name];
					if (cur) lives[ch.name] = { ...cur, seconds, bytes };
				}
			});
			const cur = lives[ch.name];
			if (cur) lives[ch.name] = { ...cur, handle };
		} catch (e) {
			lives[ch.name] = {
				status: 'error',
				seconds: 0,
				bytes: 0,
				error: e instanceof Error ? e.message : String(e),
				handle: null
			};
		}
	}

	function stopLive(ch: Channel) {
		lives[ch.name]?.handle?.stop();
	}

	$effect(() => {
		return () => {
			for (const u of objectUrls) URL.revokeObjectURL(u);
			objectUrls = [];
			for (const l of Object.values(lives)) l.handle?.stop();
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

				{#if isLivePcm(ch)}
					{@const live = lives[ch.name]}
					<div class="mt-2 flex flex-wrap items-center gap-2">
						{#if !live || live.status === 'ended' || live.status === 'stopped' || live.status === 'error'}
							<Button
								variant="outline"
								size="sm"
								disabled={!executionId}
								onclick={() => playLive(ch)}
								title={executionId
									? 'Stream and play this channel live (Web Audio) while the step produces'
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
								· {live.seconds.toFixed(1)}s · {(live.bytes / 1024).toFixed(0)} KB
							</span>
						{:else if live && live.status === 'error'}
							<span class="text-sm text-red-500">{live.error}</span>
						{/if}
					</div>
				{/if}
			</div>
		{/each}
	</div>
</section>
