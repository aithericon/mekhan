<!--
  On-graph live media preview that rides a workflow EDGE in the instance/run
  view. Given an `EdgeFeed` (resolved by `WorkflowGraphView` and handed down via
  the edge-feed context to `DeletableEdge`), it renders the live bytes flowing
  out of the source channel:
    - VIDEO  — `plan.kind === 'mse' && mediaKind === 'video'` → `<video>` driven
      by `playMseStream` (fragmented MP4/WebM appended into a MediaSource).
    - MJPEG  — `plan.kind === 'mjpeg'` → `<img>` driven by `playMjpegStream`
      (each JPEG frame swapped in).
    - AUDIO  — `plan.kind === 'pcm'` OR (`plan.kind === 'mse' && mediaKind ===
      'audio'`) → a PASSIVE scrolling waveform `<canvas>` (amplitude only, no
      sound) via `startWaveform`. A play/unmute control CLAIMS the shared
      `audioExclusivity` store to make THIS edge audible (PCM → `playLivePcm` to
      speakers; MSE-audio → raise the waveform's gain). A video's audio track is
      governed by the SAME store: unmuting a video claims, muting/teardown
      releases. Activating any audio edge steals sound from the previous owner.

  Bytes come through the shared `liveTapRegistry` (ONE source read per
  execution+channel, fanned out), so the instance panel's "Play live" and this
  edge widget share a single network read.

  GATING — we only open a passive subscription when ALL hold:
    1. the channel is live (`runtime.opened && !runtime.closed`),
    2. the widget is in-viewport (IntersectionObserver),
    3. the canvas zoom is at/above an LOD threshold (too zoomed-out → the widget
       is a few px, not worth a decoder), AND
    4. a shared slot is available under `liveFeedCap` (module singleton, ~6).
  When any fails we render a small dot / "available" badge instead. Clicking the
  badge FORCE-requests a slot (the user explicitly wants this feed), bypassing
  the auto-gate's cap check for this widget. Releasing happens on teardown,
  off-viewport, channel close, or zoom-out.
-->
<script lang="ts">
	import { onDestroy } from 'svelte';
	import { useStore } from '@xyflow/svelte';
	import { subscribe as subscribeLiveTap } from '$lib/channels/liveTapRegistry';
	import { playMseStream, type LiveMediaHandle, type LiveMediaStatus } from '$lib/channels/mseStreamPlayer';
	import { playMjpegStream } from '$lib/channels/mjpegStreamPlayer';
	import { playTextStream, tailCap } from '$lib/channels/textStreamPlayer';
	import {
		playUrdfStream,
		type UrdfJointFrame,
		type UrdfStreamHandle
	} from '$lib/channels/urdfStreamPlayer';
	import {
		playSceneStream,
		type SceneFrame,
		type SceneStreamHandle
	} from '$lib/channels/sceneStreamPlayer';
	import RobotArmTwin from './RobotArmTwin.svelte';
	import SceneTwin from './SceneTwin.svelte';
	import { subscribe as subscribeLiveTapForPcm } from '$lib/channels/liveTapRegistry';
	import { playLivePcm, parseSampleRate, type LivePcmHandle } from '$lib/audio/livePcmPlayer';
	import { startWaveform, type WaveformHandle } from '$lib/channels/audioWaveform';
	import {
		claim as claimAudio,
		release as releaseAudio,
		currentOwner as audioOwner
	} from '$lib/channels/audioExclusivity.svelte';
	import {
		request as requestSlot,
		release as releaseSlot,
		subscribe as subscribeCap
	} from '$lib/channels/liveFeedCap';
	import { edgeFeedLifecycle, type EdgeFeed } from './edge-feed-context';

	let { feed }: { feed: EdgeFeed } = $props();

	// Below this zoom the widget is too small to be worth a decoder → badge only.
	const ZOOM_LOD_THRESHOLD = 0.55;

	const store = useStore();
	const zoom = $derived(store.viewport.zoom);

	// 3D-twin render resolution. xyflow scales the whole pane with a CSS transform,
	// which the WebGL canvas buffer can't see (ResizeObserver ignores transforms), so
	// a buffer sized once at mount blurs when the graph is zoomed in. Track the zoom
	// in the pixel ratio so the buffer matches the on-screen size; clamp so a deep
	// zoom on a retina display doesn't allocate an enormous buffer.
	const twinDpr = $derived(
		Math.min((typeof window !== 'undefined' ? window.devicePixelRatio : 1) * Math.max(zoom, 1), 4)
	);

	// Presentation lifecycle (idle / live / ended). LIVE is driven by the PRODUCER
	// STEP being `running` — NOT `runtime.opened` — because a data channel's bytes
	// flow out-of-band, so per-element tokens never enter the net marking (the open
	// place reads 0 the whole time it streams). The `?follow=1` tap delivers bytes
	// exactly while the producer runs, so that's the right liveness signal.
	const lifecycle = $derived(edgeFeedLifecycle(feed.runtime, feed.terminal, feed.producerStatus));
	const ended = $derived(lifecycle === 'ended');
	const channelLive = $derived(lifecycle === 'live');

	let inViewport = $state(false);
	let status = $state<LiveMediaStatus | 'idle'>('idle');

	// Media element refs (only one is used per feed, decided by the plan).
	let videoEl = $state<HTMLVideoElement | null>(null);
	let imgEl = $state<HTMLImageElement | null>(null);
	let canvasEl = $state<HTMLCanvasElement | null>(null);
	let urdfEl = $state<HTMLDivElement | null>(null);
	let sceneEl = $state<HTMLDivElement | null>(null);
	let preEl = $state<HTMLPreElement | null>(null);
	let rootEl = $state<HTMLDivElement | null>(null);

	// Latest joint-state frame from the URDF stream; fed into the 3D twin.
	let urdfFrame = $state<UrdfJointFrame | null>(null);
	// Latest planning-scene snapshot from the scene stream; fed into the 3D twin.
	let sceneFrame = $state<SceneFrame | null>(null);
	// Accumulated tail of a live text feed (capped so a long feed can't grow
	// this widget's state without bound; the cap comfortably fills the frame).
	const EDGE_TEXT_CAP = 4000;
	let edgeText = $state('');

	let muted = $state(true);

	// This widget's audio-owner identity in the shared exclusivity store. The edge
	// id is unique per feed, so it's a natural owner key.
	const audioId = $derived(`edge-audio:${feed.edgeId}`);

	// Passive waveform (audio plans) + active PCM playback handle (PCM only).
	let waveform: WaveformHandle | null = null;
	let pcmPlayer: LivePcmHandle | null = null;

	// Slot/subscription state. `holdsSlot` tracks our `liveFeedCap` grant so
	// `releaseSlot` is called exactly once. `forced` means the user clicked the
	// badge to demand a feed even when the auto-gate would deny (still capped by
	// an explicit request()).
	let holdsSlot = $state(false);
	let forced = $state(false);
	let player: LiveMediaHandle | null = null;
	let urdfPlayer: UrdfStreamHandle | null = null;
	let scenePlayer: SceneStreamHandle | null = null;
	let release: (() => void) | null = null;
	// Guards the mount-triggered start (so we open the stream exactly once per
	// slot grant, when both the slot is held and the target element is bound).
	let streamStarted = $state(false);
	// End-state: once `ended` flips true while we held a live feed we FREEZE the
	// last frame on screen (element stays mounted) and release the tap + cap slot.
	// `frozen` keeps the element rendered so the badge never replaces the frame.
	let frozen = $state(false);

	const isVideo = $derived(feed.plan?.kind === 'mse' && feed.plan.mediaKind === 'video');
	const isMjpeg = $derived(feed.plan?.kind === 'mjpeg');
	const isPcmAudio = $derived(feed.plan?.kind === 'pcm');
	const isMseAudio = $derived(feed.plan?.kind === 'mse' && feed.plan.mediaKind === 'audio');
	const isAudio = $derived(isPcmAudio || isMseAudio);
	// A robot joint-state stream → a live 3D URDF twin (rendered in a Threlte canvas).
	const isUrdf = $derived(feed.plan?.kind === 'urdf');
	// A planning-scene stream → a live 3D twin of the arm + collision objects.
	const isScene = $derived(feed.plan?.kind === 'scene');
	// A text/* stream → a live appending console tail.
	const isText = $derived(feed.plan?.kind === 'text');
	// Anything with a live render path renders here now (video, mjpeg, audio, urdf, scene, text).
	const renderable = $derived(isVideo || isMjpeg || isAudio || isUrdf || isScene || isText);
	// A data-plane binary channel with NO live renderer → show a minimal liveness
	// dot only (no decode, no media element, no cap slot). Distinct from the
	// media widget and from a non-feed edge (which renders nothing at all).
	const isBinaryOnly = $derived(feed.plan === null);

	// Whether THIS widget currently owns audio (active ring + control state).
	const owningAudio = $derived(audioOwner() === audioId);

	// The auto-gate: everything except the cap (the cap is consulted at request()).
	const wantsLive = $derived(renderable && channelLive && inViewport && zoom >= ZOOM_LOD_THRESHOLD);

	// Tear down only the AUDIBLE path (active PCM playback / MSE gain), keeping the
	// passive waveform drawing. Used both on voluntary mute and when another edge
	// steals the audio. Also releases our ownership of the exclusivity store.
	function stopAudible(releaseOwnership = true) {
		pcmPlayer?.stop();
		pcmPlayer = null;
		// MSE waveform owns its own gain — drop it back to silent passive.
		waveform?.setAudible(false);
		if (videoEl) videoEl.muted = true;
		muted = true;
		if (releaseOwnership) releaseAudio(audioId);
	}

	function teardownStream() {
		// Audible output first (also clears exclusivity ownership), then visuals.
		stopAudible();
		waveform?.stop();
		waveform = null;
		player?.stop();
		player = null;
		urdfPlayer?.stop();
		urdfPlayer = null;
		scenePlayer?.stop();
		scenePlayer = null;
		release?.();
		release = null;
		if (holdsSlot) {
			releaseSlot();
			holdsSlot = false;
		}
		streamStarted = false;
		status = 'idle';
	}

	// FREEZE the last frame on end-of-stream: stop the byte pumps and release the
	// tap + cap slot (so other edges can reuse the slot), but DELIBERATELY keep
	// the media element mounted with its last decoded content — do NOT clear
	// video `src` / img / canvas. We never auto-load a durable/replay stream.
	//   - video : pause (keeps the last decoded frame), mute, drop audio ownership,
	//   - mjpeg : the <img> simply holds its last frame (no action needed),
	//   - audio : flatten the waveform to baseline (clear to a flat line).
	function freezeStream() {
		if (frozen) return;
		frozen = true;
		stopAudible(); // silence + release exclusivity ownership
		waveform?.stop();
		waveform = null;
		player?.stop();
		player = null;
		// URDF twin: stop the byte pump but keep `urdfFrame` so the arm freezes at
		// its last pose (the component stays mounted, holding the final joint angles).
		urdfPlayer?.stop();
		urdfPlayer = null;
		// Scene twin: same — stop the pump but keep `sceneFrame` so the arm + objects
		// freeze at the last planning-scene snapshot.
		scenePlayer?.stop();
		scenePlayer = null;
		// Video: pause but keep the frame (do not clear src).
		if (videoEl) {
			try {
				videoEl.pause?.();
			} catch {
				/* ignore */
			}
		}
		// Audio: flatten the canvas to a baseline line so it reads as "done".
		flattenWaveform();
		release?.();
		release = null;
		if (holdsSlot) {
			releaseSlot();
			holdsSlot = false;
		}
		streamStarted = false;
		status = 'idle';
	}

	// Draw a single flat baseline across the waveform canvas (end-state visual).
	function flattenWaveform() {
		const c = canvasEl;
		if (!c) return;
		const ctx = c.getContext('2d');
		if (!ctx) return;
		const w = c.width || c.clientWidth || 320;
		const h = c.height || c.clientHeight || 120;
		ctx.clearRect(0, 0, w, h);
		ctx.globalAlpha = 0.5;
		ctx.fillStyle = '#6b7280';
		ctx.fillRect(0, h / 2 - 0.5, w, 1);
		ctx.globalAlpha = 1;
	}

	function startStream() {
		const plan = feed.plan;
		if (!plan) return; // binary-only feed: no decoder, just the liveness dot.
		// Element must be mounted (render happens before this effect re-runs).
		if (isVideo && !videoEl) return;
		if (isMjpeg && !imgEl) return;
		if (isAudio && !canvasEl) return;
		if (isUrdf && !urdfEl) return;
		if (isScene && !sceneEl) return;
		if (isText && !preEl) return;
		try {
			if (isAudio && canvasEl) {
				// Audio plans render a PASSIVE waveform; the tap subscription is owned
				// by `startWaveform` (it opens its own registry sink). No `release`
				// here — the waveform's stop() releases its tap.
				waveform = startWaveform(canvasEl, {
					executionId: feed.executionId,
					channelName: feed.channelName,
					plan
				});
				status = 'streaming';
				return;
			}
			const sub = subscribeLiveTap(feed.executionId, feed.channelName, plan.kind);
			release = sub.release;
			if (isVideo && videoEl) {
				player = playMseStream({
					stream: sub.stream,
					mimeType: plan.mime,
					media: videoEl,
					onStatus: (s) => (status = s)
				});
			} else if (isMjpeg && imgEl) {
				player = playMjpegStream({
					stream: sub.stream,
					img: imgEl,
					mime: plan.mime,
					onStatus: (s) => (status = s)
				});
			} else if (isUrdf) {
				// Joint-state NDJSON → latest pose into the 3D twin. Loss-tolerant:
				// only the freshest frame matters, so no MSE-style backpressure.
				urdfPlayer = playUrdfStream({
					stream: sub.stream,
					onFrame: (f) => (urdfFrame = f),
					onStatus: (s) => (status = (s.startsWith('error') ? 'error' : s) as typeof status)
				});
			} else if (isScene) {
				// Planning-scene NDJSON → latest snapshot into the 3D twin (arm + objects).
				// Loss-tolerant like the joint-state path: only the freshest frame matters.
				scenePlayer = playSceneStream({
					stream: sub.stream,
					onFrame: (f) => (sceneFrame = f),
					onStatus: (s) => (status = (s.startsWith('error') ? 'error' : s) as typeof status)
				});
			} else if (isText) {
				// text/* → decode UTF-8 and append into the console tail (capped).
				edgeText = '';
				player = playTextStream({
					stream: sub.stream,
					onText: (t) => (edgeText = tailCap(edgeText + t, EDGE_TEXT_CAP)),
					onStatus: (s) => (status = s)
				});
			}
			status = 'streaming';
		} catch {
			status = 'error';
			teardownStream();
		}
	}

	// IntersectionObserver tracks viewport visibility (LOD/visibility gate input).
	$effect(() => {
		const el = rootEl;
		if (!el || typeof IntersectionObserver === 'undefined') {
			inViewport = true; // SSR/jsdom: assume visible, the other gates still apply.
			return;
		}
		const obs = new IntersectionObserver(
			(entries) => {
				for (const e of entries) inViewport = e.isIntersecting;
			},
			{ threshold: 0.01 }
		);
		obs.observe(el);
		return () => obs.disconnect();
	});

	// When a slot frees up elsewhere, re-attempt if we want live but were denied.
	$effect(() => {
		const unsub = subscribeCap(() => {
			if (!holdsSlot && wantsLive && requestSlot()) {
				holdsSlot = true;
			}
		});
		return unsub;
	});

	// END-STATE freeze. When `ended` flips true while we were rendering a frame
	// (held a slot), freeze it in place rather than letting the teardown effect
	// drop us back to the badge. This runs BEFORE (and pre-empts) the gate effect
	// because it sets `frozen`, which that effect checks.
	$effect(() => {
		if (ended && holdsSlot && !frozen) {
			freezeStream();
		}
	});

	// Open / close the live tap as the gate + forced flag change. We acquire a
	// slot here; a denied auto-attempt leaves us on the badge until the user
	// clicks (forced) or capacity frees (the cap subscription above).
	$effect(() => {
		// Re-run when these change.
		const want = wantsLive || (forced && renderable && channelLive);
		// Once frozen (end-state) we hold the last frame; the freeze effect already
		// released the tap + slot, so don't run teardown (which would unmount it).
		if (frozen) return;
		if (want && !holdsSlot) {
			// `forced` bypasses the auto-cap denial by still calling request() —
			// it's an explicit user demand, but we never exceed the hard cap.
			if (requestSlot()) {
				holdsSlot = true;
			}
		} else if (!want && holdsSlot && !ended) {
			// Voluntary stand-down (scrolled off-viewport, zoomed out) while still
			// live → tear down. But if we lost `want` BECAUSE the feed ended, do NOT
			// teardown here — that would race the freeze effect and snap the last
			// frame to a badge. The freeze effect owns the end-state (keeps the frame
			// mounted, releases the tap + slot).
			teardownStream();
			forced = false;
		}
	});

	// Start the stream once we hold a slot AND the target element is mounted.
	// Setting `holdsSlot` above schedules the `{#if showWidget}` render; this
	// reactive read of the element ref fires after it binds, so we never call a
	// player with a null element (which the old synchronous start risked).
	$effect(() => {
		const el = isAudio
			? canvasEl
			: isVideo
				? videoEl
				: isMjpeg
					? imgEl
					: isUrdf
						? urdfEl
						: isScene
							? sceneEl
							: isText
								? preEl
								: null;
		if (holdsSlot && renderable && el && !streamStarted) {
			streamStarted = true;
			startStream();
		}
	});

	// Pin the text console to its tail as new text lands — the natural reading
	// position for a live feed (runs after the DOM reflects the new text).
	$effect(() => {
		void edgeText;
		if (preEl) preEl.scrollTop = preEl.scrollHeight;
	});

	onDestroy(() => teardownStream());

	function onBadgeClick() {
		// Ended feeds never re-open (no replay on the edge) — the badge is inert.
		if (ended) return;
		// Explicit user demand for this feed. Try for a slot immediately; if the
		// cap is full this no-ops until one frees (cap subscription re-attempts).
		forced = true;
	}

	// Make THIS edge the sole audio owner. Steals sound from the previous owner
	// via the exclusivity store (its registered stop callback fires), then turns
	// on this widget's audible path:
	//   - video    → unmute the <video> element,
	//   - mse-audio → raise the waveform's gain (unmute the analysis element),
	//   - pcm      → start a dedicated playLivePcm to the speakers.
	function activateAudio() {
		// `claimAudio` synchronously stops whoever held audio before us.
		claimAudio(audioId, () => stopAudible(false));
		muted = false;
		if (isVideo && videoEl) {
			videoEl.muted = false;
			void videoEl.play?.()?.catch?.(() => {});
		} else if (isMseAudio) {
			waveform?.setAudible(true);
		} else if (isPcmAudio && feed.plan && !pcmPlayer) {
			// A second, dedicated tap drives the speakers (the passive waveform keeps
			// its own silent tap for visuals). The shared registry de-dupes the read.
			const sub = subscribeLiveTapForPcm(feed.executionId, feed.channelName, 'pcm');
			pcmPlayer = playLivePcm({
				stream: sub.stream,
				sampleRate: parseSampleRate(feed.plan.mime),
				onStatus: (s) => {
					if (s === 'stopped' || s === 'ended' || s === 'error') sub.release();
				}
			});
		}
	}

	// Toggle this edge's sound on/off. Off ⇒ release ownership + silence.
	function toggleAudio() {
		if (owningAudio) {
			stopAudible(); // releases ownership
		} else {
			activateAudio();
		}
	}

	// Keep the frame mounted while live (holdsSlot) AND while frozen (end-state),
	// so the last decoded frame stays on screen instead of snapping to a badge.
	const showWidget = $derived(renderable && (holdsSlot || frozen));
	const audioLabel = $derived(owningAudio ? 'Mute' : 'Unmute');
</script>

<div bind:this={rootEl} class="edge-media" data-testid="edge-media-widget">
	{#if showWidget}
		<div
			class="frame"
			class:audio={isAudio}
			class:text={isText}
			class:active={owningAudio}
			class:ended={frozen}
		>
			{#if isVideo}
				<!-- svelte-ignore a11y_media_has_caption -->
				<video bind:this={videoEl} {muted} playsinline autoplay class="media"></video>
				{#if !frozen}
					<button
						class="ctrl"
						onclick={toggleAudio}
						aria-label={audioLabel}
						title={audioLabel}
					>
						{owningAudio ? '🔊' : '🔇'}
					</button>
				{/if}
			{:else if isMjpeg}
				<img bind:this={imgEl} class="media" alt={`Live ${feed.channelName}`} />
			{:else if isUrdf}
				<div bind:this={urdfEl} class="media urdf">
					<RobotArmTwin
							robotModel={feed.robotModel}
							frame={urdfFrame}
							{frozen}
							viewKey={feed.edgeId}
							dpr={twinDpr}
						/>
				</div>
			{:else if isScene}
				<div bind:this={sceneEl} class="media urdf">
					<SceneTwin
							robotModel={feed.robotModel}
							frame={sceneFrame}
							{frozen}
							viewKey={feed.edgeId}
							dpr={twinDpr}
						/>
				</div>
			{:else if isText}
				<pre
					bind:this={preEl}
					class="console"
					aria-label={`Live ${feed.channelName} text feed`}>{edgeText}</pre>
			{:else if isAudio}
				<canvas
					bind:this={canvasEl}
					class="wave"
					width="320"
					height="120"
					aria-label={`Live ${feed.channelName} audio waveform`}
				></canvas>
				{#if !frozen}
					<button
						class="ctrl"
						onclick={toggleAudio}
						aria-label={audioLabel}
						title={owningAudio
							? 'Mute (this edge owns audio)'
							: 'Play audio (steals from others)'}
					>
						{owningAudio ? '🔊' : '▶'}
					</button>
				{/if}
			{/if}
			{#if frozen}
				<div class="overlay ended-overlay">ended</div>
			{:else if status === 'error'}
				<div class="overlay">error</div>
			{/if}
		</div>
	{:else if isBinaryOnly}
		<!-- Data-plane binary channel with no live renderer: liveness only. A tiny
		     byte dot — distinct from the media widget, no decode, no element. -->
		<span
			class="binary-dot"
			class:live={channelLive}
			class:ended
			title={ended
				? `${feed.channelName} (binary, ended)`
				: channelLive
					? `${feed.channelName} (binary, streaming)`
					: `${feed.channelName} (binary, idle)`}
			aria-label={`Binary channel ${feed.channelName}`}
		></span>
	{:else}
		<button
			class="badge"
			class:live={channelLive}
			class:ended
			onclick={onBadgeClick}
			title={ended
				? `${feed.channelName} (ended)`
				: channelLive
					? `Show live ${feed.channelName}`
					: `${feed.channelName} (idle)`}
			aria-label={ended
				? `Feed ended for ${feed.channelName}`
				: `Show live feed for ${feed.channelName}`}
		>
			<span class="dot"></span>
			<span class="lbl">{lifecycle}</span>
		</button>
	{/if}
</div>

<style>
	.edge-media {
		pointer-events: all;
	}
	.frame {
		position: relative;
		width: 320px;
		height: 180px;
		border-radius: 6px;
		overflow: hidden;
		background: #000;
		border: 1px solid hsl(var(--border));
		box-shadow: 0 1px 6px rgba(0, 0, 0, 0.35);
	}
	/* Audio waveform frame is shorter (no video raster) and dark. */
	.frame.audio {
		height: 120px;
		background: #0b0f0a;
	}
	/* Text console frame: same short form factor, terminal-dark. */
	.frame.text {
		height: 120px;
		background: #0a0e14;
	}
	.console {
		width: 100%;
		height: 100%;
		margin: 0;
		padding: 6px 8px;
		overflow-y: auto;
		font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
		font-size: 10px;
		line-height: 1.45;
		color: #d1e7dd;
		white-space: pre-wrap;
		word-break: break-word;
		/* The edge-label container centers content; a console reads left-aligned. */
		text-align: left;
	}
	/* The edge that currently OWNS audio gets a glowing ring. */
	.frame.active {
		border-color: #a3e635;
		box-shadow:
			0 0 0 2px rgba(163, 230, 53, 0.45),
			0 1px 6px rgba(0, 0, 0, 0.35);
	}
	.media {
		width: 100%;
		height: 100%;
		object-fit: cover;
		display: block;
	}
	.wave {
		width: 100%;
		height: 100%;
		display: block;
	}
	.ctrl {
		position: absolute;
		bottom: 2px;
		right: 2px;
		width: 20px;
		height: 20px;
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 11px;
		line-height: 1;
		border: none;
		border-radius: 4px;
		background: rgba(0, 0, 0, 0.55);
		color: #fff;
		cursor: pointer;
		padding: 0;
	}
	/* Frozen end-state: desaturate + dim the held frame so it reads as past. */
	.frame.ended {
		filter: grayscale(0.5) brightness(0.8);
		border-color: hsl(var(--border));
		box-shadow: 0 1px 4px rgba(0, 0, 0, 0.3);
	}
	.overlay {
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 10px;
		color: #fca5a5;
		background: rgba(0, 0, 0, 0.6);
	}
	/* The "ended" overlay sits at the corner, not full-cover, so the frozen
	   frame stays visible beneath it. */
	.overlay.ended-overlay {
		inset: auto 0 0 auto;
		padding: 1px 5px;
		border-top-left-radius: 4px;
		color: hsl(var(--muted-foreground));
		background: rgba(0, 0, 0, 0.65);
		letter-spacing: 0.04em;
	}
	/* Tiny liveness dot for a binary channel with no live renderer — no media. */
	.binary-dot {
		display: inline-block;
		width: 8px;
		height: 8px;
		border-radius: 2px; /* square-ish to read as "raw bytes", not a media dot */
		background: hsl(var(--muted-foreground));
		border: 1px solid hsl(var(--border));
	}
	.binary-dot.live {
		background: #38bdf8;
		box-shadow: 0 0 0 2px rgba(56, 189, 248, 0.25);
	}
	.binary-dot.ended {
		opacity: 0.5;
		box-shadow: none;
	}
	.badge {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		padding: 2px 6px;
		border-radius: 999px;
		border: 1px solid hsl(var(--border));
		background: hsl(var(--background));
		color: hsl(var(--muted-foreground));
		font-size: 10px;
		line-height: 1;
		cursor: pointer;
	}
	.badge .dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: hsl(var(--muted-foreground));
	}
	.badge.live .dot {
		background: #ef4444;
		box-shadow: 0 0 0 2px rgba(239, 68, 68, 0.25);
	}
	.badge.live {
		color: hsl(var(--foreground));
	}
	/* Ended badge: muted, non-pulsing — the run is over, nothing to request. */
	.badge.ended {
		cursor: default;
		opacity: 0.7;
	}
	.badge.ended .dot {
		background: hsl(var(--muted-foreground));
		box-shadow: none;
	}
</style>
