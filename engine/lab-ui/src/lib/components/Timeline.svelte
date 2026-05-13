<script lang="ts">
	import type { PersistedEvent } from '$lib/api/client';
	import { SkipBack, ChevronLeft, ChevronRight, SkipForward, Play, Pause, StepForward, Moon } from '@lucide/svelte';

	interface Props {
		events: PersistedEvent[];
		currentIndex: number;
		onIndexChange: (index: number) => void;
		evaluating?: boolean;
		runMode?: string;
		onEvaluate?: () => void;
		onToggleRunMode?: () => void;
		onHibernate?: () => void;
	}

	let { events, currentIndex, onIndexChange, evaluating, runMode, onEvaluate, onToggleRunMode, onHibernate }: Props = $props();

	function handleSlider(e: Event) {
		const target = e.target as HTMLInputElement;
		onIndexChange(parseInt(target.value, 10));
	}

	function stepBack() {
		if (currentIndex > 0) {
			onIndexChange(currentIndex - 1);
		}
	}

	function stepForward() {
		if (currentIndex < events.length - 1) {
			onIndexChange(currentIndex + 1);
		}
	}

	function jumpToStart() {
		onIndexChange(0);
	}

	function jumpToEnd() {
		onIndexChange(events.length - 1);
	}

	const currentEvent = $derived(events[currentIndex]);
</script>

<div class="timeline bg-card border-t border-border p-4">
	<div class="flex items-center gap-4">
		<div class="flex gap-1.5">
			<button
				class="p-1.5 flex items-center justify-center bg-secondary hover:bg-accent rounded disabled:opacity-50"
				onclick={jumpToStart}
				disabled={currentIndex <= 0}
				title="Jump to start"
			>
				<SkipBack class="w-4 h-4" />
			</button>
			<button
				class="p-1.5 flex items-center justify-center bg-secondary hover:bg-accent rounded disabled:opacity-50"
				onclick={stepBack}
				disabled={currentIndex <= 0}
				title="Step back"
			>
				<ChevronLeft class="w-4 h-4" />
			</button>
			<button
				class="p-1.5 flex items-center justify-center bg-secondary hover:bg-accent rounded disabled:opacity-50"
				onclick={stepForward}
				disabled={currentIndex >= events.length - 1}
				title="Step forward"
			>
				<ChevronRight class="w-4 h-4" />
			</button>
			<button
				class="p-1.5 flex items-center justify-center bg-secondary hover:bg-accent rounded disabled:opacity-50"
				onclick={jumpToEnd}
				disabled={currentIndex >= events.length - 1}
				title="Jump to end"
			>
				<SkipForward class="w-4 h-4" />
			</button>
		</div>

		<input
			type="range"
			min="0"
			max={Math.max(0, events.length - 1)}
			value={currentIndex}
			oninput={handleSlider}
			class="timeline-slider flex-1"
		/>

		<span class="text-sm font-medium text-foreground/80 tabular-nums min-w-24 text-right">
			Event <span class="text-primary">{currentIndex + 1}</span> / {events.length}
		</span>

		{#if onEvaluate && onToggleRunMode}
			<div class="flex items-center gap-2 border-l border-border pl-3 ml-auto">
				<button
					class="px-2.5 py-1 text-sm rounded flex items-center gap-1.5 bg-primary text-primary-foreground hover:bg-primary/90 disabled:bg-muted disabled:cursor-not-allowed"
					disabled={evaluating || runMode === 'running'}
					onclick={onEvaluate}
				>
					<StepForward class="w-3.5 h-3.5" />
					<span>{evaluating ? 'Evaluating...' : 'Evaluate'}</span>
				</button>

				<button
					class="px-2.5 py-1 text-sm rounded flex items-center gap-1.5 {runMode === 'running'
						? 'bg-red-600 hover:bg-red-500'
						: 'bg-emerald-600 hover:bg-emerald-500'}"
					onclick={onToggleRunMode}
				>
					{#if runMode === 'running'}
						<Pause class="w-3.5 h-3.5" />
						<span>Stop</span>
					{:else}
						<Play class="w-3.5 h-3.5" />
						<span>Run</span>
					{/if}
				</button>

				{#if runMode === 'running'}
					<span class="text-xs text-emerald-400 animate-pulse">Auto-running...</span>
				{/if}

				{#if onHibernate}
					<button
						class="px-2.5 py-1 text-sm rounded flex items-center gap-1.5 border border-amber-600/50 text-amber-500 hover:bg-amber-600/20"
						onclick={onHibernate}
						title="Force hibernate this net (free memory, keep events in NATS)"
					>
						<Moon class="w-3.5 h-3.5" />
						<span>Hibernate</span>
					</button>
				{/if}
			</div>
		{/if}
	</div>

	{#if currentEvent}
		<div class="mt-2 text-xs text-muted-foreground">
			<span class="font-medium">{currentEvent.event.type}</span>
			<span class="ml-2">{new Date(currentEvent.timestamp).toLocaleTimeString()}</span>
		</div>
	{/if}
</div>
