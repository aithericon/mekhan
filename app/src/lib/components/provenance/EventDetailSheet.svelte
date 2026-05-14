<script lang="ts">
	import { X, ChevronDown, ChevronRight, ArrowRight, Workflow, Maximize2, Minimize2 } from '@lucide/svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { Separator } from '$lib/components/ui/separator';
	import CopyButton from '$lib/components/ui/copy-button/CopyButton.svelte';
	import MonacoEditor from '$lib/components/petri/MonacoEditor.svelte';
	import ArtifactCard from '$lib/components/catalogue/ArtifactCard.svelte';
	import { getEventDetail, type EventDetail, type TokenInfo } from '$lib/api/client';
	import type { ProvenanceGraphNode } from '$lib/types/provenance';
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
	let expandedTokens = $state<Record<string, boolean>>({});
	let fullscreen = $state<{ title: string; value: string } | null>(null);

	$effect(() => {
		if (node && open) {
			loadDetail(node.net_id, node.event_seq);
			expandedTokens = {};
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

	function stringify(v: unknown): string {
		try {
			return JSON.stringify(v, null, 2);
		} catch {
			return String(v);
		}
	}

	function hasPayload(token: TokenInfo): boolean {
		return token.data !== null && token.data !== undefined;
	}

	function toggleToken(id: string) {
		expandedTokens = { ...expandedTokens, [id]: !expandedTokens[id] };
	}

	function openFullscreen(title: string, value: string) {
		fullscreen = { title, value };
	}

	function closeFullscreen() {
		fullscreen = null;
	}

	const color = $derived(node ? getNodeColor(node) : '#6b7280');

	// Decide which payload sections to show based on event type.
	const showEffectResult = $derived(
		!!detail &&
			(detail.event_type === 'EffectCompleted' || detail.event_type === 'EffectFailed') &&
			detail.effect_result !== null &&
			detail.effect_result !== undefined
	);
	const showBridge = $derived(!!detail?.bridge);
	const showSignalSource = $derived(!!detail?.signal_dispatch);

	// For signal-injected TokenCreated, pull the produced token's data as the
	// signal payload (that's what the signal listener placed into the token).
	const signalPayload = $derived(
		detail?.event_type === 'TokenCreated'
			? detail.tokens.find((t) => t.role === 'produced' && hasPayload(t))?.data ?? null
			: null
	);

	// Extract executor_submit surface data from effect_result for a dedicated block.
	const execSummary = $derived.by(() => {
		if (!detail || detail.effect_handler !== 'executor_submit') return null;
		const r = detail.effect_result as Record<string, unknown> | null;
		if (!r || typeof r !== 'object') return null;
		return {
			execution_id: (r.execution_id as string | undefined) ?? null,
			signal_key: (r.signal_key as string | undefined) ?? null
		};
	});
</script>

{#if open}
<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="fixed inset-0 z-50 flex justify-end" onclick={onclose}>
	<!-- Panel -->
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="h-full w-[520px] max-w-full overflow-y-auto border-l bg-white shadow-xl dark:bg-zinc-900"
		onclick={(e) => e.stopPropagation()}
	>
		<!-- Header -->
		<div class="flex items-center justify-between border-b px-4 py-3">
			<div>
				{#if node}
					<div class="flex items-center gap-2">
						<span class="inline-block h-3 w-3 rounded-full" style="background: {color}"></span>
						<span class="font-semibold">{node.transition_name ?? node.event_type}</span>
						{#if node.effect_handler}
							<Badge variant="outline" class="text-[10px]">{node.effect_handler}</Badge>
						{/if}
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
			<div class="flex items-center justify-center py-12 text-zinc-400">Loading…</div>
		{:else if error}
			<div class="rounded-md bg-red-50 p-3 text-sm text-red-700 dark:bg-red-900/20 dark:text-red-300">
				{error}
			</div>
		{:else if detail}

			<!-- Signal source (TokenCreated with a known dispatcher) -->
			{#if showSignalSource && detail.signal_dispatch}
				<section class="mb-4 rounded-md border border-amber-200 bg-amber-50 p-3 dark:border-amber-900 dark:bg-amber-900/20">
					<div class="flex items-center gap-2 text-xs font-semibold uppercase tracking-wider text-amber-800 dark:text-amber-200">
						<ArrowRight class="h-3.5 w-3.5" />
						Signal Source
					</div>
					<div class="mt-1.5 text-sm">
						Emitted by
						<span class="font-mono font-medium">{detail.signal_dispatch.dispatch_net}</span>
						#<span class="font-mono font-medium">{detail.signal_dispatch.dispatch_seq}</span>
					</div>
					<div class="mt-1 flex items-center gap-1 text-xs text-zinc-500 dark:text-zinc-400">
						<span class="font-mono truncate">{detail.signal_dispatch.signal_key}</span>
						<CopyButton text={detail.signal_dispatch.signal_key} />
					</div>
				</section>
			{/if}

			<!-- Signal payload (for TokenCreated via signal) -->
			{#if signalPayload !== null}
				{@const payloadJson = stringify(signalPayload)}
				<section class="mb-4">
					<div class="mb-2 flex items-center justify-between">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-zinc-500">
							Signal Payload
						</h4>
						<button
							class="rounded p-1 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-600 dark:hover:bg-zinc-800"
							onclick={() => openFullscreen('Signal Payload', payloadJson)}
							title="Expand to fullscreen"
						>
							<Maximize2 class="h-3.5 w-3.5" />
						</button>
					</div>
					<MonacoEditor value={payloadJson} language="json" height="320px" />
				</section>
			{/if}

			<!-- Bridge target (for TokenBridgedOut) -->
			{#if showBridge && detail.bridge}
				<section class="mb-4 rounded-md border border-orange-200 bg-orange-50 p-3 dark:border-orange-900 dark:bg-orange-900/20">
					<div class="flex items-center gap-2 text-xs font-semibold uppercase tracking-wider text-orange-800 dark:text-orange-200">
						<Workflow class="h-3.5 w-3.5" />
						Bridge Target
					</div>
					<div class="mt-1.5 font-mono text-sm">
						{detail.bridge.target_net} / {detail.bridge.target_place}
					</div>
				</section>
			{/if}

			<!-- executor_submit summary -->
			{#if execSummary && (execSummary.execution_id || execSummary.signal_key)}
				<section class="mb-4">
					<h4 class="mb-2 text-xs font-semibold uppercase tracking-wider text-zinc-500">
						Execution
					</h4>
					<div class="space-y-1 rounded-md border p-3 text-sm">
						{#if execSummary.execution_id}
							<div class="flex items-center gap-2">
								<span class="text-zinc-500">execution_id</span>
								<span class="font-mono text-xs truncate">{execSummary.execution_id}</span>
								<CopyButton text={execSummary.execution_id} />
							</div>
						{/if}
						{#if execSummary.signal_key}
							<div class="flex items-center gap-2">
								<span class="text-zinc-500">signal_key</span>
								<span class="font-mono text-xs truncate">{execSummary.signal_key}</span>
								<CopyButton text={execSummary.signal_key} />
							</div>
						{/if}
					</div>
				</section>
			{/if}

			<!-- Effect result (JSON) -->
			{#if showEffectResult}
				{@const resultJson = stringify(detail.effect_result)}
				{@const resultTitle = detail.event_type === 'EffectFailed' ? 'Failure' : 'Effect Result'}
				<section class="mb-4">
					<div class="mb-2 flex items-center justify-between">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-zinc-500">
							{resultTitle}
						</h4>
						<button
							class="rounded p-1 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-600 dark:hover:bg-zinc-800"
							onclick={() => openFullscreen(resultTitle, resultJson)}
							title="Expand to fullscreen"
						>
							<Maximize2 class="h-3.5 w-3.5" />
						</button>
					</div>
					<MonacoEditor value={resultJson} language="json" height="420px" />
				</section>
			{/if}

			<!-- Tokens -->
			<section>
				<h4 class="text-xs font-semibold uppercase tracking-wider text-zinc-500">Tokens</h4>
				<div class="mt-2 space-y-1">
					{#each detail.tokens as token}
						{@const rowId = `${token.role}:${token.token_id}`}
						{@const expanded = expandedTokens[rowId]}
						<div class="rounded bg-zinc-50 dark:bg-zinc-800">
							<div class="flex items-center gap-2 px-2 py-1 text-sm">
								<Badge variant="outline" class="text-[10px]">{token.role}</Badge>
								<span class="font-mono text-xs truncate">{token.place_name ?? token.place_id}</span>
								<span class="ml-auto font-mono text-[10px] text-zinc-400 truncate">
									{token.token_id.slice(0, 8)}
								</span>
								<CopyButton text={token.token_id} />
								{#if hasPayload(token)}
									<button
										class="rounded p-0.5 text-zinc-400 hover:bg-zinc-200 hover:text-zinc-700 dark:hover:bg-zinc-700"
										onclick={() => toggleToken(rowId)}
										title={expanded ? 'Hide payload' : 'Show payload'}
									>
										{#if expanded}
											<ChevronDown class="h-3.5 w-3.5" />
										{:else}
											<ChevronRight class="h-3.5 w-3.5" />
										{/if}
									</button>
								{/if}
							</div>
							{#if expanded && hasPayload(token)}
								{@const tokenJson = stringify(token.data)}
								<div class="px-2 pb-2">
									<div class="mb-1 flex justify-end">
										<button
											class="rounded p-1 text-zinc-400 hover:bg-zinc-200 hover:text-zinc-700 dark:hover:bg-zinc-700"
											onclick={() =>
												openFullscreen(`Token · ${token.place_name ?? token.place_id}`, tokenJson)}
											title="Expand to fullscreen"
										>
											<Maximize2 class="h-3.5 w-3.5" />
										</button>
									</div>
									<MonacoEditor value={tokenJson} language="json" height="240px" />
								</div>
							{/if}
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
							{#if detail.task.assignee}
								<span>&middot; {detail.task.assignee}</span>
							{/if}
							{#if detail.task.completed_at}
								<span>&middot; completed {formatTime(detail.task.completed_at)}</span>
							{/if}
						</div>
						{#if detail.task.detail && Object.keys(detail.task.detail).length > 0}
							{@const taskJson = stringify(detail.task.detail)}
							<div class="mt-2">
								<div class="mb-1 flex justify-end">
									<button
										class="rounded p-1 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-600 dark:hover:bg-zinc-800"
										onclick={() => openFullscreen('Task Detail', taskJson)}
										title="Expand to fullscreen"
									>
										<Maximize2 class="h-3.5 w-3.5" />
									</button>
								</div>
								<MonacoEditor value={taskJson} language="json" height="260px" />
							</div>
						{/if}
					</div>
				</section>
			{/if}

			<!-- Catalogue artifact: use the full ArtifactCard -->
			{#if detail.artifact}
				<Separator class="my-4" />
				<section>
					<h4 class="mb-2 text-xs font-semibold uppercase tracking-wider text-zinc-500">Artifact</h4>
					<ArtifactCard entry={detail.artifact} expanded />
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
							<div class="flex items-center justify-between rounded bg-zinc-50 px-2 py-1 text-sm dark:bg-zinc-800">
								<span class="font-mono">{metric.key}</span>
								<span class="tabular-nums font-medium">{metric.value}</span>
							</div>
						{/each}
						{#if detail.metrics.length > 20}
							<div class="px-2 text-xs text-zinc-400">
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
					<div class="mt-2 max-h-64 space-y-1 overflow-y-auto">
						{#each detail.logs as log}
							<div class="rounded bg-zinc-50 px-2 py-1 font-mono text-xs dark:bg-zinc-800">
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

<!-- Fullscreen JSON viewer — a higher-z-index overlay on top of the sheet. -->
{#if fullscreen}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="fixed inset-0 z-[60] flex flex-col bg-white dark:bg-zinc-900"
		onclick={closeFullscreen}
	>
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div
			class="flex h-full w-full flex-col"
			onclick={(e) => e.stopPropagation()}
		>
			<div class="flex items-center justify-between border-b px-4 py-3">
				<div class="font-semibold">{fullscreen.title}</div>
				<div class="flex items-center gap-1">
					<CopyButton text={fullscreen.value} />
					<button
						class="rounded-md p-1 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-600 dark:hover:bg-zinc-800"
						onclick={closeFullscreen}
						title="Exit fullscreen"
					>
						<Minimize2 class="h-4 w-4" />
					</button>
					<button
						class="rounded-md p-1 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-600 dark:hover:bg-zinc-800"
						onclick={closeFullscreen}
						title="Close"
					>
						<X class="h-4 w-4" />
					</button>
				</div>
			</div>
			<div class="flex-1 overflow-hidden">
				<MonacoEditor value={fullscreen.value} language="json" height="calc(100vh - 57px)" />
			</div>
		</div>
	</div>
{/if}
