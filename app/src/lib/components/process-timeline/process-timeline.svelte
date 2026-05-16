<script lang="ts">
	import type { ProcessTimelineEntry } from '$lib/types/process';
	import * as Tooltip from '$lib/components/ui/tooltip';

	let {
		entries
	}: {
		entries: ProcessTimelineEntry[];
	} = $props();

	function formatTimestamp(ts?: string): string {
		if (!ts) return '';
		const d = new Date(ts);
		const now = new Date();
		const diffMs = now.getTime() - d.getTime();
		const diffMin = Math.floor(diffMs / 60000);
		if (diffMin < 1) return 'just now';
		if (diffMin < 60) return `${diffMin}m ago`;
		const diffHour = Math.floor(diffMin / 60);
		if (diffHour < 24) return `${diffHour}h ago`;
		return d.toLocaleDateString();
	}

	function formatFullTimestamp(ts: string): string {
		return new Date(ts).toLocaleString(undefined, {
			weekday: 'short',
			year: 'numeric',
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		});
	}

	function formatDuration(ms: number): string {
		if (ms < 1000) return `${ms}ms`;
		const secs = Math.floor(ms / 1000);
		if (secs < 60) return `${secs}s`;
		const mins = Math.floor(secs / 60);
		const remSecs = secs % 60;
		if (mins < 60) return `${mins}m ${remSecs}s`;
		const hours = Math.floor(mins / 60);
		const remMins = mins % 60;
		return `${hours}h ${remMins}m`;
	}
</script>

<div class="space-y-0">
	{#each entries as entry, index (entry.step)}
		{@const isLast = index === entries.length - 1}
		{@const ts = entry.completed_at ?? entry.started_at}
		<div class="relative flex items-start gap-3">
			<!-- Vertical connector line -->
			{#if !isLast}
				<div
					class="absolute top-[22px] -bottom-1 w-0.5 bg-border/60"
					style="left: calc(5rem + 0.75rem + 11px)"
				></div>
			{/if}

			<!-- Timestamp column -->
			<div class="flex h-[22px] w-20 shrink-0 items-center justify-end">
				{#if ts}
					<Tooltip.Root>
						<Tooltip.Trigger
							class="cursor-default text-sm tabular-nums text-muted-foreground"
						>
							{formatTimestamp(ts)}
						</Tooltip.Trigger>
						<Tooltip.Content>
							<p>{formatFullTimestamp(ts)}</p>
						</Tooltip.Content>
					</Tooltip.Root>
				{/if}
			</div>

			<!-- Status dot -->
			<div class="relative z-10 flex size-[22px] shrink-0 items-center justify-center">
				{#if entry.status === 'completed'}
					<div
						class="flex size-[22px] items-center justify-center rounded-full bg-emerald-500 text-white"
					>
						<svg class="size-3" viewBox="0 0 12 12" fill="none">
							<path
								d="M2.5 6L5 8.5L9.5 3.5"
								stroke="currentColor"
								stroke-width="1.5"
								stroke-linecap="round"
								stroke-linejoin="round"
							/>
						</svg>
					</div>
				{:else if entry.status === 'running'}
					<div
						class="flex size-[22px] items-center justify-center rounded-full border-2 border-cyan-500 bg-card"
					>
						<div class="size-2.5 animate-pulse rounded-full bg-cyan-500"></div>
					</div>
				{:else if entry.status === 'failed'}
					<div
						class="flex size-[22px] items-center justify-center rounded-full bg-red-500 text-white"
					>
						<svg class="size-3" viewBox="0 0 12 12" fill="none">
							<path
								d="M3 3L9 9M9 3L3 9"
								stroke="currentColor"
								stroke-width="1.5"
								stroke-linecap="round"
							/>
						</svg>
					</div>
				{:else if entry.status === 'skipped'}
					<div
						class="flex size-[22px] items-center justify-center rounded-full border-2 border-dashed border-muted-foreground/40 bg-card text-muted-foreground"
					>
						<svg class="size-3" viewBox="0 0 12 12" fill="none">
							<path
								d="M3 6h6"
								stroke="currentColor"
								stroke-width="1.5"
								stroke-linecap="round"
							/>
						</svg>
					</div>
				{:else}
					<div
						class="flex size-[22px] items-center justify-center rounded-full border-2 border-border bg-card"
					>
						<div class="size-2 rounded-full bg-muted-foreground/30"></div>
					</div>
				{/if}
			</div>

			<!-- Content -->
			<div class="min-w-0 flex-1 pb-4">
				<div class="flex items-center gap-2">
					<span
						class={`text-sm font-medium ${entry.status === 'pending' ? 'text-muted-foreground' : 'text-foreground'}`}
					>
						{entry.label}
					</span>
					{#if entry.human}
						<span
							class="rounded-full bg-amber-100 px-1.5 py-0.5 text-sm font-medium text-amber-700 dark:bg-amber-900 dark:text-amber-200"
						>
							human
						</span>
					{/if}
				</div>

				{#if entry.duration_ms || entry.iterations}
					<p class="mt-0.5 text-sm text-muted-foreground">
						{#if entry.iterations}
							{entry.completed_iterations ?? 0}/{entry.iterations} iterations
							{#if entry.duration_ms} · {formatDuration(entry.duration_ms)}{/if}
						{:else if entry.duration_ms}
							{formatDuration(entry.duration_ms)}
						{/if}
					</p>
				{/if}

				{#if entry.iterations && entry.status === 'running'}
					{@const pct = ((entry.completed_iterations ?? 0) / entry.iterations) * 100}
					<div class="mt-1.5 h-1.5 w-full max-w-[240px] overflow-hidden rounded-full bg-muted/50">
						<div
							class="h-full rounded-full bg-cyan-500 transition-all duration-300"
							style="width: {pct}%"
						></div>
					</div>
				{/if}
			</div>
		</div>
	{/each}
</div>
