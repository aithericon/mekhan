<script lang="ts">
	import { X } from '@lucide/svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { Separator } from '$lib/components/ui/separator';
	import { getEventDetail } from '$lib/api/client';
	import type { EventDetail, ProvenanceGraphNode } from '$lib/types/provenance';
	import { getNodeColor } from '$lib/utils/provenance-graph';

	interface Props {
		node: ProvenanceGraphNode | null;
		open: boolean;
		onclose: () => void;
	}

	let { node, open, onclose }: Props = $props();

	let detail = $state<EventDetail | null>(null);
	let loading = $state(false);
	let error = $state<string | null>(null);

	$effect(() => {
		if (node && open) {
			loadDetail(node.net_id, node.event_seq);
		} else {
			detail = null;
		}
	});

	async function loadDetail(netId: string, eventSeq: number) {
		loading = true;
		error = null;
		try {
			detail = await getEventDetail(netId, eventSeq);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load detail';
		} finally {
			loading = false;
		}
	}

	function formatTime(ts: string): string {
		return new Intl.DateTimeFormat(undefined, {
			dateStyle: 'medium',
			timeStyle: 'medium'
		}).format(new Date(ts));
	}

	const color = $derived(node ? getNodeColor(node) : '#6b7280');
</script>

{#if open}
<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="fixed inset-0 z-50 flex justify-end" onclick={onclose}>
	<!-- Panel -->
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="h-full w-[480px] max-w-full overflow-y-auto border-l bg-white shadow-xl dark:bg-zinc-900"
		onclick={(e) => e.stopPropagation()}
	>
		<!-- Header -->
		<div class="flex items-center justify-between border-b px-4 py-3">
			<div>
				{#if node}
					<div class="flex items-center gap-2">
						<span class="inline-block h-3 w-3 rounded-full" style="background: {color}"></span>
						<span class="font-semibold">{node.transition_name ?? node.event_type}</span>
					</div>
					<div class="mt-0.5 text-xs text-zinc-500">
						{node.net_id} &middot; event #{node.event_seq} &middot; {formatTime(node.timestamp)}
					</div>
				{/if}
			</div>
			<button
				class="rounded-md p-1 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-600 dark:hover:bg-zinc-800"
				onclick={onclose}
			>
				<X class="h-4 w-4" />
			</button>
		</div>

		<div class="p-4">

		{#if loading}
			<div class="flex items-center justify-center py-12 text-zinc-400">Loading...</div>
		{:else if error}
			<div class="rounded-md bg-red-50 p-3 text-sm text-red-700 dark:bg-red-900/20 dark:text-red-300">
				{error}
			</div>
		{:else if detail}
			<!-- Tokens -->
			<section class="mt-4">
				<h4 class="text-xs font-semibold uppercase tracking-wider text-zinc-500">Tokens</h4>
				<div class="mt-2 space-y-1">
					{#each detail.tokens as token}
						<div class="flex items-center gap-2 rounded px-2 py-1 text-sm bg-zinc-50 dark:bg-zinc-800">
							<Badge variant="outline" class="text-[10px]">{token.role}</Badge>
							<span class="font-mono text-xs truncate">{token.place_name ?? token.place_id}</span>
							<span class="ml-auto font-mono text-[10px] text-zinc-400 truncate">
								{token.token_id.slice(0, 8)}
							</span>
						</div>
					{/each}
				</div>
			</section>

			<!-- Human task -->
			{#if detail.task}
				<Separator class="my-4" />
				<section>
					<h4 class="text-xs font-semibold uppercase tracking-wider text-zinc-500">Human Task</h4>
					<div class="mt-2 rounded-md border p-3">
						<div class="font-medium">{detail.task.title}</div>
						<div class="mt-1 flex items-center gap-2 text-sm text-zinc-500">
							<Badge variant={detail.task.status === 'completed' ? 'default' : 'secondary'}>
								{detail.task.status}
							</Badge>
							{#if detail.task.completed_at}
								<span>Completed {formatTime(detail.task.completed_at)}</span>
							{/if}
						</div>
					</div>
				</section>
			{/if}

			<!-- Catalogue artifact -->
			{#if detail.artifact}
				<Separator class="my-4" />
				<section>
					<h4 class="text-xs font-semibold uppercase tracking-wider text-zinc-500">Artifact</h4>
					<div class="mt-2 rounded-md border p-3">
						<div class="flex items-center gap-2">
							<Badge variant="outline">{detail.artifact.category}</Badge>
							<span class="font-medium">{detail.artifact.name}</span>
						</div>
						<div class="mt-1 text-sm text-zinc-500">
							{detail.artifact.filename}
							{#if detail.artifact.size_bytes}
								&middot; {(detail.artifact.size_bytes / 1024).toFixed(1)} KB
							{/if}
						</div>
					</div>
				</section>
			{/if}

			<!-- Metrics -->
			{#if detail.metrics.length > 0}
				<Separator class="my-4" />
				<section>
					<h4 class="text-xs font-semibold uppercase tracking-wider text-zinc-500">
						Metrics ({detail.metrics.length})
					</h4>
					<div class="mt-2 space-y-1">
						{#each detail.metrics.slice(0, 20) as metric}
							<div class="flex items-center justify-between rounded px-2 py-1 text-sm bg-zinc-50 dark:bg-zinc-800">
								<span class="font-mono">{metric.key}</span>
								<span class="tabular-nums font-medium">{metric.value}</span>
							</div>
						{/each}
						{#if detail.metrics.length > 20}
							<div class="text-xs text-zinc-400 px-2">
								+{detail.metrics.length - 20} more
							</div>
						{/if}
					</div>
				</section>
			{/if}

			<!-- Logs -->
			{#if detail.logs.length > 0}
				<Separator class="my-4" />
				<section>
					<h4 class="text-xs font-semibold uppercase tracking-wider text-zinc-500">
						Logs ({detail.logs.length})
					</h4>
					<div class="mt-2 space-y-1 max-h-64 overflow-y-auto">
						{#each detail.logs as log}
							<div class="rounded px-2 py-1 text-xs bg-zinc-50 dark:bg-zinc-800 font-mono">
								<span class="text-zinc-400">[{log.level}]</span>
								{#if log.source}
									<span class="text-zinc-500">{log.source}:</span>
								{/if}
								<span>{log.message}</span>
							</div>
						{/each}
					</div>
				</section>
			{/if}
		{/if}
		</div>
	</div>
</div>
{/if}
