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
		X
	} from '@lucide/svelte';

	interface Props {
		events: PersistedEvent[];
		currentIndex: number;
		onSelectEvent: (index: number) => void;
		onInspectEvent?: (sequence: number) => void;
		getTransitionName?: (id: string) => string;
		getPlaceName?: (id: string) => string;
	}

	let { events, currentIndex, onSelectEvent, onInspectEvent, getTransitionName, getPlaceName }: Props = $props();

	const ITEM_HEIGHT = 52;
	let containerHeight = $state(0);

	// Filter state
	let typeFilter = new SvelteSet<string>();
	let textSearch = $state('');

	const eventTypeChips = [
		{ key: 'TransitionFired', label: 'Fired', color: 'bg-amber-500/15 text-amber-500 border-amber-500/30' },
		{ key: 'EffectCompleted', label: 'Effect', color: 'bg-emerald-500/15 text-emerald-500 border-emerald-500/30' },
		{ key: 'EffectFailed', label: 'Fx Err', color: 'bg-red-500/15 text-red-500 border-red-500/30' },
		{ key: 'TokenCreated', label: 'Created', color: 'bg-green-500/15 text-green-500 border-green-500/30' },
		{ key: 'TokenBridgedOut', label: 'Bridge', color: 'bg-rose-500/15 text-rose-500 border-rose-500/30' },
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

	// Map currentIndex (in original array) to index in filtered array for highlighting
	const filteredCurrentIndex = $derived.by(() => {
		if (currentIndex < 0 || currentIndex >= events.length) return -1;
		const currentSeq = events[currentIndex].sequence;
		return filteredEvents.findIndex(e => e.sequence === currentSeq);
	});

	function handleFilteredEventClick(filteredIndex: number) {
		const event = filteredEvents[filteredIndex];
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

	// Get badge class for signal type
	function getSignalBadgeClass(signalType: string): string {
		switch (signalType) {
			case 'accepted': return 'bg-green-500/15 text-green-500';
			case 'denied': return 'bg-red-500/15 text-red-500';
			case 'confirmed': return 'bg-blue-500/15 text-blue-500';
			case 'failed': return 'bg-red-500/15 text-red-500';
			default: return 'bg-purple-500/15 text-purple-500';
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
				iconColor: 'text-blue-500'
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
				iconColor: signalType ? 'text-indigo-500' : 'text-green-500',
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
				iconColor: 'text-amber-500'
			};
		}

		if (eventType === 'TokenConsumed') {
			const placeName = getPlaceName ? getPlaceName(e.place_id) : e.place_id;
			return {
				icon: Minus,
				title: '−',
				detail: placeName ?? '',
				iconColor: 'text-red-400'
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
				iconColor: 'text-rose-500'
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
				iconColor: 'text-emerald-500'
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
				iconColor: 'text-red-500'
			};
		}

		if (eventType === 'ErrorOccurred') {
			return {
				icon: AlertCircle,
				title: 'Err',
				detail: e.message ?? 'Error',
				iconColor: 'text-red-500'
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
	<div class="px-3 py-2 border-b border-border bg-muted flex items-center justify-between">
		<h3 class="font-semibold text-foreground text-sm">Event Log</h3>
		<span class="text-xs text-muted-foreground tabular-nums">
			{#if hasFilters}
				{filteredEvents.length} / {events.length}
			{:else}
				{events.length}
			{/if}
		</span>
	</div>

	<!-- Filter bar -->
	<div class="shrink-0 px-2 py-1.5 border-b border-border bg-card/50 space-y-1">
		<div class="flex flex-wrap gap-1">
			{#each eventTypeChips as chip (chip.key)}
				<button
					class="px-1.5 py-0.5 text-[10px] rounded border transition-colors
						{typeFilter.has(chip.key) ? chip.color + ' border-current font-medium' : 'text-muted-foreground border-transparent hover:border-border'}"
					onclick={() => toggleTypeFilter(chip.key)}
				>
					{chip.label}
				</button>
			{/each}
			{#if hasFilters}
				<button
					class="px-1 py-0.5 text-muted-foreground hover:text-foreground"
					onclick={() => { typeFilter.clear(); textSearch = ''; }}
					title="Clear filters"
				>
					<X class="w-3 h-3" />
				</button>
			{/if}
		</div>
		<input
			bind:value={textSearch}
			placeholder="Search events..."
			class="w-full px-2 py-1 text-xs rounded border border-border bg-background text-foreground focus:outline-none focus:ring-1 focus:ring-primary"
		/>
	</div>

	<div class="flex-1 overflow-hidden" bind:clientHeight={containerHeight}>
		<div class="flex-1 overflow-y-auto" style="height: {containerHeight}px;">
			{#each filteredEvents as event, index (event.sequence)}
				{@const summary = getEventSummary(event.event)}
				{@const Icon = summary.icon}
				<div style="height: {ITEM_HEIGHT}px;">
					<button
						class="w-full h-full text-left px-2 py-1.5 border-b border-border hover:bg-muted transition-colors
							{index === filteredCurrentIndex ? 'bg-primary/10 border-l-2 border-l-primary' : ''}"
						onclick={() => handleFilteredEventClick(index)}
					>
						<div class="flex items-center gap-1.5">
							<span class={`flex-shrink-0 ${summary.iconColor}`}>
								<Icon class="w-3.5 h-3.5" />
							</span>
							<span class="flex-shrink-0 text-[10px] font-mono font-medium text-muted-foreground bg-muted px-1 rounded">
								{summary.title}
							</span>
							{#if summary.signalType}
								<span class="flex-shrink-0 text-[9px] font-medium px-1 rounded {summary.signalBadgeClass}">
									{summary.signalType}
								</span>
							{/if}
							<span class="text-xs text-foreground truncate flex-1" title={summary.detail}>
								{summary.detail}
							</span>
						</div>
						<div class="flex items-center gap-1 mt-0.5 ml-5">
							<span class="text-[10px] text-muted-foreground tabular-nums">
								#{event.sequence}
							</span>
							<span class="text-[10px] text-muted-foreground/50">•</span>
							<span class="text-[10px] text-muted-foreground tabular-nums">
								{formatTime(event.timestamp)}
							</span>
						</div>
					</button>
				</div>
			{/each}
		</div>
	</div>
</div>
