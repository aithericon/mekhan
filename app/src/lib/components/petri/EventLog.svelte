<script lang="ts">
	import type { PersistedEvent, Token } from '$lib/types/petri';
	import { SvelteSet } from 'svelte/reactivity';
	import {
		Zap,
		Plus,
		Minus,
		Globe,
		AlertCircle,
		Circle,
		ArrowUpRight,
		Cog,
		CircleX,
		X,
		ArrowDownWideNarrow,
		ArrowUpWideNarrow
	} from '@lucide/svelte';
	import NodeKindBadge, { type NodeKind } from './NodeKindBadge.svelte';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import CopyButton from '$lib/components/ui/copy-button/CopyButton.svelte';

	interface Props {
		events: PersistedEvent[];
		currentIndex: number;
		onSelectEvent: (index: number) => void;
		onInspectEvent?: (sequence: number) => void;
		getTransitionName?: (id: string) => string;
		getPlaceName?: (id: string) => string;
		/** Oldest events dropped from the in-memory buffer (history no longer
		 *  scrubbable). Surfaced so a long live stream reads as truncated, not lost. */
		evictedCount?: number;
	}

	let { events, currentIndex, onSelectEvent, onInspectEvent, getTransitionName, getPlaceName, evictedCount = 0 }: Props = $props();

	const ITEM_HEIGHT = 52;
	let containerHeight = $state(0);

	// Filter state
	let typeFilter = new SvelteSet<string>();
	let textSearch = $state('');

	// Sort + windowing. Events arrive in ascending sequence order; default to
	// newest-first so the latest activity is at the top of a long stream.
	let sortNewestFirst = $state(true);
	const RENDER_LIMIT = 200;
	let showAll = $state(false);

	const eventTypeChips: { key: NodeKind; label: string }[] = [
		{ key: 'TransitionFired', label: 'Fired' },
		{ key: 'EffectCompleted', label: 'Effect' },
		{ key: 'EffectFailed', label: 'Fx Err' },
		{ key: 'TokenCreated', label: 'Created' },
		{ key: 'TokenBridgedOut', label: 'Bridge' },
	];

	const hasFilters = $derived(typeFilter.size > 0 || textSearch.trim() !== '');

	// Filtered events
	const filteredEvents = $derived.by(() => {
		let result = events;
		if (typeFilter.size > 0) {
			result = result.filter(e => typeFilter.has(e.event.type));
		}
		if (textSearch.trim()) {
			const q = textSearch.toLowerCase();
			result = result.filter(e => {
				const summary = getEventSummary(e.event);
				return summary.detail.toLowerCase().includes(q);
			});
		}
		return result;
	});

	// Sorted view: filteredEvents is ascending by sequence, so reverse for
	// newest-first (cheap, no re-sort on every streamed event).
	const sortedEvents = $derived(sortNewestFirst ? filteredEvents.slice().reverse() : filteredEvents);

	// Window the rendered list so a multi-thousand-event stream doesn't mount
	// thousands of DOM nodes. With newest-first this shows the most recent N.
	const displayedEvents = $derived(showAll ? sortedEvents : sortedEvents.slice(0, RENDER_LIMIT));
	const hiddenCount = $derived(sortedEvents.length - displayedEvents.length);

	// Map currentIndex (in original array) to index in displayed array for highlighting
	const displayedCurrentIndex = $derived.by(() => {
		if (currentIndex < 0 || currentIndex >= events.length) return -1;
		const currentSeq = events[currentIndex].sequence;
		return displayedEvents.findIndex(e => e.sequence === currentSeq);
	});

	// Serialize the events the user is currently looking at (filters/search
	// applied) as pretty JSON for pasting into an AI chat. Computed lazily at
	// click time so the buffer isn't re-serialized on every event that streams
	// in. When nothing is filtered this is the whole log.
	function eventsAsJson(): string {
		return JSON.stringify(filteredEvents.map((e) => e.event), null, 2);
	}

	function handleFilteredEventClick(displayedIndex: number) {
		const event = displayedEvents[displayedIndex];
		const originalIndex = events.findIndex(e => e.sequence === event.sequence);
		if (originalIndex >= 0) {
			onSelectEvent(originalIndex);
			onInspectEvent?.(event.sequence);
		}
	}

	function toggleTypeFilter(key: string) {
		if (typeFilter.has(key)) typeFilter.delete(key);
		else typeFilter.add(key);
	}

	// Extract a meaningful identifier from token data (e.g., job_id, id, worker_id)
	function extractTokenId(token: Token | undefined): string | null {
		if (!token?.color) return null;
		const color = token.color as any;
		if (color.type !== 'Data' || !color.value) return null;
		const val = color.value;
		// Try common ID fields
		return val.job_id ?? val.id ?? val.worker_id ?? val.correlation_id ?? val.task_id ?? null;
	}

	// Check if a token is a coordination signal (has _provenance)
	function getCoordinationSignalType(token: Token | undefined): string | null {
		if (!token?.color) return null;
		const color = token.color as any;
		if (color.type !== 'Data' || !color.value) return null;
		const val = color.value;
		if (val._provenance && typeof val._provenance === 'object') {
			return val._provenance.signal_type ?? 'signal';
		}
		return null;
	}

	// Get badge class for signal type (routed through theme tokens)
	function getSignalBadgeClass(signalType: string): string {
		switch (signalType) {
			case 'accepted': return 'bg-success/15 text-success';
			case 'denied': return 'bg-destructive/15 text-destructive';
			case 'confirmed': return 'bg-info/15 text-info';
			case 'failed': return 'bg-destructive/15 text-destructive';
			default: return 'bg-secondary text-secondary-foreground';
		}
	}

	// Get a short summary for the event list
	function getEventSummary(event: PersistedEvent['event']): {
		icon: typeof Zap;
		title: string;
		detail: string;
		iconColor: string;
		signalType?: string;
		signalBadgeClass?: string;
	} {
		const e = event as any;
		const eventType = e.type as string;

		if (eventType === 'NetInitialized') {
			return {
				icon: Globe,
				title: 'Init',
				detail: 'Net initialized',
				iconColor: 'text-info'
			};
		}

		if (eventType === 'TokenCreated') {
			const placeName = getPlaceName ? getPlaceName(e.place_id) : e.place_id;
			const tokenId = extractTokenId(e.token);
			const signalType = getCoordinationSignalType(e.token);
			const detail = tokenId
				? `${tokenId} → ${placeName}`
				: (placeName ?? '');
			return {
				icon: Plus,
				title: signalType ? '📡' : '+',
				detail,
				iconColor: signalType ? 'text-info' : 'text-success',
				signalType: signalType ?? undefined,
				signalBadgeClass: signalType ? getSignalBadgeClass(signalType) : undefined
			};
		}

		if (eventType === 'TransitionFired') {
			const transitionName = getTransitionName ? getTransitionName(e.transition_id) : e.transition_id;
			const consumed = (e.consumed_tokens?.length ?? 0);
			const produced = (e.produced_tokens?.length ?? 0);

			// Try to extract a correlation ID from produced tokens
			let correlationHint = '';
			if (e.produced_tokens?.length > 0) {
				const [, firstProduced] = e.produced_tokens[0] as [string, Token];
				const id = extractTokenId(firstProduced);
				if (id) correlationHint = ` [${id}]`;
			}

			return {
				icon: Zap,
				title: `${consumed}→${produced}`,
				detail: `${transitionName}${correlationHint}`,
				iconColor: 'text-warning'
			};
		}

		if (eventType === 'TokenConsumed') {
			const placeName = getPlaceName ? getPlaceName(e.place_id) : e.place_id;
			return {
				icon: Minus,
				title: '−',
				detail: placeName ?? '',
				iconColor: 'text-destructive'
			};
		}

		if (eventType === 'TokenBridgedOut') {
			const sourcePlaceName = e.source_place_name ?? (getPlaceName ? getPlaceName(e.source_place_id) : e.source_place_id);
			const tokenId = extractTokenId(e.token);
			const detail = tokenId
				? `${tokenId} · ${sourcePlaceName} → ${e.target_net_id}/${e.target_place_name}`
				: `${sourcePlaceName} → ${e.target_net_id}/${e.target_place_name}`;
			return {
				icon: ArrowUpRight,
				title: 'OUT',
				detail,
				iconColor: 'text-destructive'
			};
		}

		if (eventType === 'EffectCompleted') {
			const transitionName = getTransitionName ? getTransitionName(e.transition_id) : e.transition_id;
			const consumed = (e.consumed_tokens?.length ?? 0);
			const produced = (e.produced_tokens?.length ?? 0);
			const handlerId = e.effect_handler_id ?? '';
			return {
				icon: Cog,
				title: `${consumed}→${produced}`,
				detail: `${transitionName} [${handlerId}]`,
				iconColor: 'text-success'
			};
		}

		if (eventType === 'EffectFailed') {
			const transitionName = getTransitionName ? getTransitionName(e.transition_id) : e.transition_id;
			const handlerId = e.effect_handler_id ?? '';
			const errorMsg = e.error_message ?? 'Effect failed';
			return {
				icon: CircleX,
				title: 'Fx!',
				detail: `${transitionName} [${handlerId}]: ${errorMsg}`,
				iconColor: 'text-destructive'
			};
		}

		if (eventType === 'ErrorOccurred') {
			return {
				icon: AlertCircle,
				title: 'Err',
				detail: e.message ?? 'Error',
				iconColor: 'text-destructive'
			};
		}

		return {
			icon: Circle,
			title: '?',
			detail: eventType,
			iconColor: 'text-muted-foreground'
		};
	}

	function formatTime(timestamp: string): string {
		const date = new Date(timestamp);
		return date.toLocaleTimeString('en-US', {
			hour12: false,
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		});
	}
</script>

<div class="event-log h-full bg-card border-l border-border overflow-hidden flex flex-col">
	<div class="px-3 py-2 border-b border-border bg-muted flex items-center justify-between gap-2">
		<h3 class="font-semibold text-foreground text-sm">Event Log</h3>
		<div class="flex items-center gap-1.5">
			<span class="text-sm text-muted-foreground tabular-nums">
				{#if hasFilters}
					{filteredEvents.length} / {events.length}
				{:else}
					{events.length}
				{/if}
			</span>
			{#if evictedCount > 0}
				<span
					class="text-sm text-muted-foreground/60 tabular-nums"
					title={`${evictedCount.toLocaleString()} older events trimmed from the in-memory buffer — they remain in the engine log but are no longer scrubbable here`}
				>
					+{evictedCount.toLocaleString()} earlier
				</span>
			{/if}
			<Button
				variant="ghost"
				size="icon-xs"
				onclick={() => (sortNewestFirst = !sortNewestFirst)}
				title={sortNewestFirst ? 'Sorted newest first — click for oldest first' : 'Sorted oldest first — click for newest first'}
				aria-label="Toggle sort order"
			>
				{#if sortNewestFirst}
					<ArrowDownWideNarrow class="w-3.5 h-3.5" />
				{:else}
					<ArrowUpWideNarrow class="w-3.5 h-3.5" />
				{/if}
			</Button>
			{#if filteredEvents.length > 0}
				<CopyButton
					getText={eventsAsJson}
					label="Copy"
					title={`Copy ${filteredEvents.length} ${hasFilters ? 'filtered' : 'latest'} event${filteredEvents.length === 1 ? '' : 's'} as JSON`}
				/>
			{/if}
		</div>
	</div>

	<!-- Filter bar -->
	<div class="shrink-0 px-2 py-1.5 border-b border-border bg-card/50 space-y-1">
		<div class="flex flex-wrap gap-1">
			{#each eventTypeChips as chip (chip.key)}
				<button
					class="rounded transition-opacity {typeFilter.has(chip.key) ? '' : 'opacity-50 hover:opacity-100'}"
					onclick={() => toggleTypeFilter(chip.key)}
					aria-pressed={typeFilter.has(chip.key)}
				>
					<NodeKindBadge kind={chip.key} label={chip.label} size="xs" />
				</button>
			{/each}
			{#if hasFilters}
				<Button
					variant="ghost"
					size="icon-xs"
					onclick={() => { typeFilter.clear(); textSearch = ''; }}
					title="Clear filters"
				>
					<X class="w-3 h-3" />
				</Button>
			{/if}
		</div>
		<Input
			bind:value={textSearch}
			placeholder="Search events..."
			class="h-7 text-sm"
		/>
	</div>

	<div class="flex-1 overflow-hidden" bind:clientHeight={containerHeight}>
		<div class="flex-1 overflow-y-auto" style="height: {containerHeight}px;">
			{#each displayedEvents as event, index (event.sequence)}
				{@const summary = getEventSummary(event.event)}
				{@const Icon = summary.icon}
				<div style="height: {ITEM_HEIGHT}px;">
					<button
						class="w-full h-full text-left px-2 py-1.5 border-b border-border hover:bg-muted transition-colors
							{index === displayedCurrentIndex ? 'bg-primary/10 border-l-2 border-l-primary' : ''}"
						onclick={() => handleFilteredEventClick(index)}
					>
						<div class="flex items-center gap-1.5">
							<span class={`flex-shrink-0 ${summary.iconColor}`}>
								<Icon class="w-3.5 h-3.5" />
							</span>
							<span class="flex-shrink-0 text-sm font-mono font-medium text-muted-foreground bg-muted px-1 rounded">
								{summary.title}
							</span>
							{#if summary.signalType}
								<span class="flex-shrink-0 text-sm font-medium px-1 rounded {summary.signalBadgeClass}">
									{summary.signalType}
								</span>
							{/if}
							<span class="text-sm text-foreground truncate flex-1" title={summary.detail}>
								{summary.detail}
							</span>
						</div>
						<div class="flex items-center gap-1 mt-0.5 ml-5">
							<span class="text-sm text-muted-foreground tabular-nums">
								#{event.sequence}
							</span>
							<span class="text-sm text-muted-foreground/50">•</span>
							<span class="text-sm text-muted-foreground tabular-nums">
								{formatTime(event.timestamp)}
							</span>
						</div>
					</button>
				</div>
			{/each}
			{#if hiddenCount > 0}
				<button
					class="w-full px-2 py-2 text-sm text-muted-foreground hover:bg-muted hover:text-foreground transition-colors border-b border-border"
					onclick={() => (showAll = true)}
				>
					Show {hiddenCount} more {sortNewestFirst ? 'older' : 'newer'} event{hiddenCount === 1 ? '' : 's'}
				</button>
			{:else if showAll && sortedEvents.length > RENDER_LIMIT}
				<button
					class="w-full px-2 py-2 text-sm text-muted-foreground hover:bg-muted hover:text-foreground transition-colors border-b border-border"
					onclick={() => (showAll = false)}
				>
					Collapse to {RENDER_LIMIT}
				</button>
			{/if}
		</div>
	</div>
</div>
