<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import FileBox from '@lucide/svelte/icons/file-box';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import SkipBack from '@lucide/svelte/icons/skip-back';
	import SkipForward from '@lucide/svelte/icons/skip-forward';
	import Maximize from '@lucide/svelte/icons/maximize';
	import MediaLightbox from './MediaLightbox.svelte';
	import ArtifactProvenance from '$lib/components/catalogue/ArtifactProvenance.svelte';
	import type { LiveArtifactEntry } from '$lib/api/client';
	import type { createProcessLiveStore } from '$lib/stores/process-live.svelte';
	import {
		pickRenderer,
		groupKey,
		groupLabel,
		stepNumber,
		isShowcaseEntry
	} from './renderers/registry';

	type Store = ReturnType<typeof createProcessLiveStore>;
	interface Props {
		store: Store;
		/**
		 * Embed mode for the Overview tab: only showcase groups (declared
		 * render hints + image/video/audio), no header and no empty-state —
		 * the host card supplies the chrome and gates on artifact presence.
		 */
		renderableOnly?: boolean;
		/**
		 * Restrict the rendered panels to these group keys (e.g.
		 * `['hint:gp-posterior']`). Used by the Report's group-mode embed block
		 * to show one render bucket. Undefined → all groups (default).
		 */
		groupFilter?: string[];
		/**
		 * Show a provenance line (step / category / size / time / producer params)
		 * under each entry. On for Report embeds; off for the Process Overview card.
		 */
		showProvenance?: boolean;
	}
	let { store, renderableOnly = false, groupFilter, showProvenance = false }: Props = $props();

	/**
	 * Per group (by render_hint / MIME / category):
	 *   - sort entries by step → created_at
	 *   - track selected index (default: latest)
	 *   - show a "new iteration available" pill when live events land while
	 *     the user is parked on an older index
	 */
	interface Group {
		key: string;
		label: string;
		entries: LiveArtifactEntry[];
	}

	// The viewer only ever shows artifacts it can actually render — files
	// without a renderer (ndjson, parquet, …) are already covered by the
	// artifact card list, so a "no renderer" placeholder here is pure noise.
	const sourceEntries = $derived(
		renderableOnly
			? store.artifacts.filter(isShowcaseEntry)
			: store.artifacts.filter((e) => pickRenderer(e) !== null)
	);

	const groups = $derived.by<Group[]>(() => {
		const m = new Map<string, LiveArtifactEntry[]>();
		for (const e of sourceEntries) {
			const k = groupKey(e);
			const arr = m.get(k) ?? [];
			arr.push(e);
			m.set(k, arr);
		}
		const out: Group[] = [];
		for (const [k, arr] of m) {
			arr.sort((a, b) => {
				const sa = stepNumber(a);
				const sb = stepNumber(b);
				if (sa !== sb) return sa - sb;
				return new Date(a.created_at).getTime() - new Date(b.created_at).getTime();
			});
			out.push({ key: k, label: groupLabel(k), entries: arr });
		}
		// Stable ordering: named render_hints first, then MIME, then category
		out.sort((a, b) => {
			const rank = (k: string) =>
				k.startsWith('hint:') ? 0 : k.startsWith('mime:') ? 1 : 2;
			const ra = rank(a.key);
			const rb = rank(b.key);
			if (ra !== rb) return ra - rb;
			return a.label.localeCompare(b.label);
		});
		return out;
	});

	// Optional group-key whitelist (group-mode embed renders one bucket).
	const visibleGroups = $derived(
		groupFilter && groupFilter.length ? groups.filter((g) => groupFilter.includes(g.key)) : groups
	);

	// Selected index per group (keyed by group.key). Defaults to latest on
	// first load; sticky when user scrubs but doesn't auto-advance.
	let selectedByGroup = $state<Record<string, number>>({});
	let stickyUserSelection = $state<Record<string, boolean>>({});

	function indexFor(g: Group): number {
		const sel = selectedByGroup[g.key];
		if (sel === undefined || !stickyUserSelection[g.key]) {
			return g.entries.length - 1;
		}
		return Math.min(sel, g.entries.length - 1);
	}

	function setIndex(g: Group, i: number) {
		selectedByGroup[g.key] = i;
		// Only mark sticky if the user picked something other than the latest;
		// otherwise keep auto-advance behavior.
		stickyUserSelection[g.key] = i !== g.entries.length - 1;
	}

	function jumpLatest(g: Group) {
		selectedByGroup[g.key] = g.entries.length - 1;
		stickyUserSelection[g.key] = false;
	}

	// Lightbox: tracks WHICH group is maximized; the selected index is the
	// same state the inline scrubber uses, so navigating in the overlay moves
	// the panel underneath (and vice versa) instead of forking a second cursor.
	let lightboxKey = $state<string | null>(null);
	const lightboxGroup = $derived(
		lightboxKey === null ? null : (groups.find((g) => g.key === lightboxKey) ?? null)
	);

	const statusDotClass = $derived(
		store.artifactStatus === 'streaming'
			? 'bg-green-500'
			: store.artifactStatus === 'error'
				? 'bg-red-500'
				: 'bg-amber-500'
	);
	const statusLabel = $derived(
		store.artifactStatus === 'streaming'
			? 'live'
			: store.artifactStatus === 'reconnecting'
				? 'reconnecting…'
				: store.artifactStatus === 'loading'
					? 'loading…'
					: store.artifactStatus
	);
</script>

<section class="flex flex-col gap-4 {renderableOnly ? '' : 'mb-6'}">
	<!-- Header + empty-state only when there's something to say: artifacts
	     that exist but have no renderer live in the card list below, so the
	     viewer disappears entirely rather than apologizing per file. -->
	{#if !renderableOnly && (groups.length > 0 || store.artifacts.length === 0)}
		<div class="flex items-center justify-between">
			<div class="flex items-center gap-2">
				<FileBox class="size-4 text-muted-foreground" />
				<h3 class="text-sm font-medium">Live artifact viewer</h3>
				<span class="inline-block size-2 rounded-full {statusDotClass}"></span>
				<span class="text-sm text-muted-foreground">{statusLabel}</span>
			</div>
			<p class="text-sm text-muted-foreground">
				{sourceEntries.length} renderable artifact{sourceEntries.length === 1 ? '' : 's'}
			</p>
		</div>
	{/if}

	{#if visibleGroups.length === 0}
		{#if !renderableOnly && store.artifacts.length === 0}
			<div
				class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-8"
			>
				<p class="text-sm text-muted-foreground">
					No artifacts yet — they'll appear here as the process produces them.
				</p>
			</div>
		{/if}
	{:else}
		{#each visibleGroups as g (g.key)}
			{@const idx = indexFor(g)}
			{@const entry = g.entries[idx]}
			{@const Renderer = pickRenderer(entry)}
			{@const isLatest = idx === g.entries.length - 1}
			<div class="flex flex-col gap-2 rounded-xl border border-border bg-background p-3">
				<div class="flex flex-wrap items-center justify-between gap-2">
					<div class="flex items-center gap-2">
						<Badge variant="secondary" class="font-mono text-sm">{g.label}</Badge>
						<span class="text-sm text-muted-foreground">{entry.name}</span>
						{#if entry.process_step}
							<Badge variant="outline" class="text-sm">step {entry.process_step}</Badge>
						{/if}
					</div>

					<div class="flex items-center gap-1">
						{#if g.entries.length > 1}
							{#if !isLatest && stickyUserSelection[g.key]}
								<Button size="sm" variant="ghost" onclick={() => jumpLatest(g)}>
									Jump to latest ({g.entries.length})
								</Button>
							{/if}
							<Button
								variant="ghost"
								size="icon-sm"
								disabled={idx === 0}
								title="First"
								onclick={() => setIndex(g, 0)}
							>
								<SkipBack class="size-4" />
							</Button>
							<Button
								variant="ghost"
								size="icon-sm"
								disabled={idx === 0}
								title="Previous"
								onclick={() => setIndex(g, idx - 1)}
							>
								<ChevronLeft class="size-4" />
							</Button>
							<input
								type="range"
								min="0"
								max={g.entries.length - 1}
								step="1"
								value={idx}
								oninput={(e) => setIndex(g, parseInt(e.currentTarget.value, 10))}
								class="w-56 accent-primary"
							/>
							<Button
								variant="ghost"
								size="icon-sm"
								disabled={idx === g.entries.length - 1}
								title="Next"
								onclick={() => setIndex(g, idx + 1)}
							>
								<ChevronRight class="size-4" />
							</Button>
							<Button
								variant="ghost"
								size="icon-sm"
								disabled={idx === g.entries.length - 1}
								title="Latest"
								onclick={() => setIndex(g, g.entries.length - 1)}
							>
								<SkipForward class="size-4" />
							</Button>
							<span class="ml-1 text-sm tabular-nums text-muted-foreground">
								{idx + 1} / {g.entries.length}
							</span>
						{/if}
						<Button
							variant="ghost"
							size="icon-sm"
							title="Enlarge"
							onclick={() => (lightboxKey = g.key)}
						>
							<Maximize class="size-4" />
						</Button>
					</div>
				</div>

				{#if Renderer}
					<Renderer {entry} />
				{/if}
				{#if showProvenance}
					<ArtifactProvenance {entry} class="px-0.5" />
				{/if}
			</div>
		{/each}
	{/if}

	{#if store.error}
		<p class="text-sm text-red-500">{store.error}</p>
	{/if}
</section>

{#if lightboxGroup}
	<MediaLightbox
		entries={lightboxGroup.entries}
		index={indexFor(lightboxGroup)}
		label={lightboxGroup.label}
		onnavigate={(i) => setIndex(lightboxGroup, i)}
		onclose={() => (lightboxKey = null)}
	/>
{/if}
