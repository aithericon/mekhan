<script lang="ts">
	import { onMount } from 'svelte';
	import { previewCron } from '$lib/api/client';

	let { schedule, timezone }: { schedule: string; timezone: string } = $props();

	let upcoming = $state<string[]>([]);
	let error = $state<string | null>(null);
	let pending = $state(false);

	// Debounced re-fetch. Editing a cron string char-by-char shouldn't pummel
	// the API, but we want the preview to feel live — 400 ms is the sweet
	// spot between "instant" and "noisy".
	let timer: ReturnType<typeof setTimeout> | null = null;

	$effect(() => {
		const s = schedule;
		const tz = timezone;
		if (timer) clearTimeout(timer);
		timer = setTimeout(() => {
			void fetchPreview(s, tz);
		}, 400);
	});

	onMount(() => {
		void fetchPreview(schedule, timezone);
		return () => {
			if (timer) clearTimeout(timer);
		};
	});

	async function fetchPreview(s: string, tz: string) {
		if (!s) {
			upcoming = [];
			error = null;
			return;
		}
		pending = true;
		try {
			const body = await previewCron({ schedule: s, timezone: tz, count: 5 });
			if (body.error) {
				error = body.error;
				upcoming = [];
			} else {
				error = null;
				upcoming = body.upcoming ?? [];
			}
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
			upcoming = [];
		} finally {
			pending = false;
		}
	}

	function format(iso: string): string {
		try {
			const d = new Date(iso);
			return d.toLocaleString(undefined, {
				dateStyle: 'medium',
				timeStyle: 'short'
			});
		} catch {
			return iso;
		}
	}
</script>

<div class="rounded-md border border-border/60 bg-muted/20 p-2 space-y-1">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Next fires</span>
		{#if pending}
			<span class="text-sm text-muted-foreground/70">…</span>
		{/if}
	</div>
	{#if error}
		<p class="text-sm text-destructive">{error}</p>
	{:else if upcoming.length === 0}
		<p class="text-sm italic text-muted-foreground">No upcoming fires.</p>
	{:else}
		<ul class="space-y-0.5">
			{#each upcoming as iso, i (i)}
				<li class="text-sm text-foreground">{format(iso)}</li>
			{/each}
		</ul>
	{/if}
</div>
