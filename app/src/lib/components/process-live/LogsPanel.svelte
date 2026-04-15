<script lang="ts">
	import { tick } from 'svelte';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Badge } from '$lib/components/ui/badge';
	import ScrollText from '@lucide/svelte/icons/scroll-text';
	import type { createProcessLiveStore } from '$lib/stores/process-live.svelte';

	type Store = ReturnType<typeof createProcessLiveStore>;
	interface Props {
		store: Store;
	}
	let { store }: Props = $props();

	let levelFilter = $state<string>('all');
	let searchInput = $state<string>('');
	let signalKeyInput = $state<string>('');
	let followTail = $state<boolean>(true);

	let scroller: HTMLDivElement | undefined = $state();

	const logLevelColors: Record<string, string> = {
		info: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		warn: 'bg-amber-100 text-amber-800 dark:bg-amber-900 dark:text-amber-200',
		error: 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200',
		debug: 'bg-slate-100 text-slate-600 dark:bg-slate-800 dark:text-slate-300'
	};

	function logLevelColor(l: string): string {
		return logLevelColors[l.toLowerCase()] ?? logLevelColors.debug;
	}

	function formatTimestamp(s: string): string {
		return new Intl.DateTimeFormat(undefined, {
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		}).format(new Date(s));
	}

	function applyLevel(level: string) {
		levelFilter = level;
		store.setLogFilter({
			level: level === 'all' ? undefined : level,
			query: searchInput.trim() || undefined
		});
	}

	function applySearch() {
		store.setLogFilter({
			level: levelFilter === 'all' ? undefined : levelFilter,
			query: searchInput.trim() || undefined
		});
	}

	function applySignalKey() {
		store.setSignalKey(signalKeyInput.trim() || undefined);
	}

	// Auto-scroll to bottom as new logs arrive, if follow-tail is on.
	$effect(() => {
		const _ = store.logs.length;
		if (!followTail || !scroller) return;
		tick().then(() => {
			if (scroller) scroller.scrollTop = scroller.scrollHeight;
		});
	});

	const statusLabel = $derived(
		store.logStatus === 'streaming'
			? 'live'
			: store.logStatus === 'reconnecting'
				? 'reconnecting…'
				: store.logStatus === 'loading'
					? 'loading…'
					: store.logStatus
	);
	const statusDotClass = $derived(
		store.logStatus === 'streaming'
			? 'bg-green-500'
			: store.logStatus === 'error'
				? 'bg-red-500'
				: 'bg-amber-500'
	);
</script>

<div class="flex flex-col gap-3">
	<!-- Top row: status + level filters + follow-tail -->
	<div class="flex flex-wrap items-center gap-3">
		<div class="flex items-center gap-1 text-xs">
			<span class="inline-block size-2 rounded-full {statusDotClass}"></span>
			<span class="text-muted-foreground">{statusLabel}</span>
		</div>

		<div class="flex items-center gap-1">
			{#each ['all', 'info', 'warn', 'error'] as level}
				<Button
					variant={levelFilter === level ? 'default' : 'ghost'}
					size="sm"
					onclick={() => applyLevel(level)}
				>
					{level.charAt(0).toUpperCase() + level.slice(1)}
				</Button>
			{/each}
		</div>

		<label class="flex cursor-pointer items-center gap-1 text-xs text-muted-foreground">
			<input type="checkbox" bind:checked={followTail} class="size-3.5 accent-primary" />
			Follow tail
		</label>
	</div>

	<!-- Filters row -->
	<div class="flex flex-wrap items-center gap-2">
		<Input
			placeholder="search message…"
			class="h-8 w-64 text-xs"
			bind:value={searchInput}
			onkeydown={(e: KeyboardEvent) => e.key === 'Enter' && applySearch()}
		/>
		<Button size="sm" variant="outline" onclick={applySearch}>Search</Button>

		<Input
			placeholder="signal_key (optional)"
			class="h-8 w-52 text-xs"
			bind:value={signalKeyInput}
			onkeydown={(e: KeyboardEvent) => e.key === 'Enter' && applySignalKey()}
		/>
		<Button size="sm" variant="outline" onclick={applySignalKey}>Drill-down</Button>
	</div>

	<!-- Log scroller -->
	{#if store.logs.length === 0 && store.logStatus !== 'loading'}
		<div
			class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-12"
		>
			<ScrollText class="size-8 text-muted-foreground/40" />
			<p class="mt-2 text-sm text-muted-foreground">No logs in window</p>
		</div>
	{:else}
		<div
			bind:this={scroller}
			class="max-h-[60vh] overflow-y-auto rounded-lg border border-border bg-card"
		>
			<div class="divide-y divide-border">
				{#each store.logs as log (log.id)}
					<div class="flex items-start gap-2 px-4 py-1.5 text-xs hover:bg-accent/30">
						<span class="shrink-0 pt-0.5 tabular-nums text-muted-foreground">
							{formatTimestamp(log.timestamp)}
						</span>
						<Badge class={logLevelColor(log.level)} variant="secondary">
							{log.level}
						</Badge>
						{#if log.source}
							<span class="shrink-0 pt-0.5 font-mono text-muted-foreground">{log.source}</span>
						{/if}
						<span class="break-all pt-0.5 text-foreground">{log.message}</span>
					</div>
				{/each}
			</div>
		</div>
		<p class="text-xs text-muted-foreground">{store.logs.length} entries</p>
	{/if}

	{#if store.error}
		<p class="text-xs text-red-500">{store.error}</p>
	{/if}
</div>
