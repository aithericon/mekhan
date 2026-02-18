<script lang="ts">
	import { page } from '$app/state';
	import { getInstance, getInstanceState, cancelInstance } from '$lib/api/client';
	import type { WorkflowInstance, InstanceState } from '$lib/types/api';
	import type { PersistedEvent } from '$lib/types/petri';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import Activity from '@lucide/svelte/icons/activity';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import CircleDot from '@lucide/svelte/icons/circle-dot';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import Zap from '@lucide/svelte/icons/zap';
	import { SvelteSet } from 'svelte/reactivity';

	const instanceId = $derived(page.params.id!);

	let instance = $state<WorkflowInstance | null>(null);
	let instanceState = $state<InstanceState | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let expandedPlaces = new SvelteSet<string>();
	let showEventLog = $state(false);
	let expandedEvents = new SvelteSet<number>();

	const statusColors: Record<string, string> = {
		created: 'bg-gray-100 text-gray-700',
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700',
		cancelled: 'bg-slate-100 text-slate-700'
	};

	const eventTypeColors: Record<string, string> = {
		TokenCreated: 'text-green-600',
		TransitionFired: 'text-blue-600',
		EffectCompleted: 'text-blue-600',
		EffectFailed: 'text-red-600',
		TokenConsumed: 'text-orange-600',
		TokenRemoved: 'text-orange-600',
		TokenUpdated: 'text-violet-600',
		NetInitialized: 'text-gray-500',
		NetCreated: 'text-gray-500',
		NetCompleted: 'text-green-700',
		NetCancelled: 'text-slate-600',
		ErrorOccurred: 'text-red-700',
		TokenBridgedOut: 'text-cyan-600'
	};

	const markingTokens = $derived(instanceState?.marking?.tokens ?? {});

	const hasTokens = $derived(
		Object.values(markingTokens).some((tokens) => tokens.length > 0)
	);

	function togglePlace(placeId: string) {
		if (expandedPlaces.has(placeId)) {
			expandedPlaces.delete(placeId);
		} else {
			expandedPlaces.add(placeId);
		}
	}

	function toggleEvent(seq: number) {
		if (expandedEvents.has(seq)) {
			expandedEvents.delete(seq);
		} else {
			expandedEvents.add(seq);
		}
	}

	function eventSummary(event: PersistedEvent): string {
		const e = event.event;
		switch (e.type) {
			case 'TokenCreated':
				return e.place_name ?? e.place_id;
			case 'TransitionFired':
				return e.transition_name ?? e.transition_id;
			case 'EffectCompleted':
				return `${e.transition_name ?? e.transition_id} (${e.effect_handler_id})`;
			case 'EffectFailed':
				return `${e.transition_name ?? e.transition_id}: ${e.error_message}`;
			case 'TokenConsumed':
			case 'TokenRemoved':
				return e.place_id;
			case 'TokenUpdated':
				return `${e.place_id} → ${e.new_color.type}`;
			case 'NetCompleted':
				return e.terminal_place_id;
			case 'NetCancelled':
				return e.reason ?? '';
			case 'ErrorOccurred':
				return e.message;
			default:
				return '';
		}
	}

	async function load() {
		loading = true;
		error = null;
		try {
			instance = await getInstance(instanceId);
			if (instance.status !== 'created') {
				instanceState = await getInstanceState(instanceId);
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load instance';
		} finally {
			loading = false;
		}
	}

	async function refresh() {
		if (!instance) return;
		try {
			instance = await getInstance(instanceId);
			if (instance.status !== 'created') {
				instanceState = await getInstanceState(instanceId);
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to refresh';
		}
	}

	async function handleCancel() {
		if (!instance || !confirm('Cancel this instance?')) return;
		try {
			await cancelInstance(instance.id);
			instance = { ...instance, status: 'cancelled' };
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to cancel';
		}
	}

	const formatDate = (s: string | null) => (s ? new Date(s).toLocaleString() : '-');

	$effect(() => {
		load();
	});
</script>

<div class="h-full overflow-y-auto" data-testid="instance-page">
	<div class="mx-auto max-w-3xl px-6 py-8">
		{#if loading}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Loading...
			</div>
		{:else if error}
			<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
				{error}
			</div>
		{:else if instance}
			<div class="mb-6 flex items-start justify-between">
				<div>
					<div class="flex items-center gap-2">
						<h1 class="text-2xl font-semibold tracking-tight text-foreground" data-testid="instance-heading">Instance</h1>
						<Badge class={statusColors[instance.status] ?? ''} variant="secondary">
							{instance.status}
						</Badge>
						{#if instanceState?.engine}
							{@const engine = instanceState.engine}
							<Badge
								data-testid="engine-status"
								class={engine.available
									? 'bg-emerald-100 text-emerald-700'
									: 'bg-gray-100 text-gray-500'}
								variant="secondary"
								title={engine.available
									? `Engine hot — ${engine.run_mode ?? 'unknown'}`
									: 'Engine offline'}
							>
								{engine.available ? 'Engine hot' : 'Engine offline'}
							</Badge>
						{/if}
					</div>
					<p class="mt-1 font-mono text-xs text-muted-foreground">{instance.net_id}</p>
				</div>
				<div class="flex items-center gap-2">
					<Button variant="outline" size="sm" onclick={refresh}>
						<RefreshCw class="size-3.5" />
						Refresh
					</Button>
					{#if instance.status === 'running' || instance.status === 'created'}
						<Button
							variant="outline"
							size="sm"
							class="border-destructive/30 text-destructive hover:bg-destructive/10"
							onclick={handleCancel}
						>
							Cancel
						</Button>
					{/if}
				</div>
			</div>

			<div class="space-y-4">
				<div class="rounded-lg border border-border bg-card">
					<div class="border-b border-border px-4 py-2.5">
						<span class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
							Details
						</span>
					</div>
					<dl class="divide-y divide-border">
						<div class="flex justify-between px-4 py-2.5">
							<dt class="text-xs text-muted-foreground">Instance ID</dt>
							<dd class="font-mono text-xs text-foreground">{instance.id}</dd>
						</div>
						<div class="flex justify-between px-4 py-2.5">
							<dt class="text-xs text-muted-foreground">Template</dt>
							<dd class="text-xs text-foreground">
								<a href="/templates/{instance.template_id}" class="text-primary underline">
									v{instance.template_version}
								</a>
							</dd>
						</div>
						<div class="flex justify-between px-4 py-2.5">
							<dt class="text-xs text-muted-foreground">Created</dt>
							<dd class="text-xs text-foreground">{formatDate(instance.created_at)}</dd>
						</div>
						<div class="flex justify-between px-4 py-2.5">
							<dt class="text-xs text-muted-foreground">Started</dt>
							<dd class="text-xs text-foreground">{formatDate(instance.started_at)}</dd>
						</div>
						<div class="flex justify-between px-4 py-2.5">
							<dt class="text-xs text-muted-foreground">Completed</dt>
							<dd class="text-xs text-foreground">{formatDate(instance.completed_at)}</dd>
						</div>
						{#if instance.current_step}
							<div class="flex justify-between px-4 py-2.5">
								<dt class="text-xs text-muted-foreground">Current Step</dt>
								<dd class="text-xs font-medium text-foreground">{instance.current_step}</dd>
							</div>
						{/if}
					</dl>
				</div>

				{#if instance.status !== 'created'}
					<!-- Marking -->
					<div class="rounded-lg border border-border bg-card" data-testid="marking-section">
						<div class="border-b border-border px-4 py-2.5">
							<span class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
								Marking
							</span>
							{#if instanceState?.event_count != null}
								<span class="ml-2 text-[10px] text-muted-foreground">
									{instanceState.event_count} events
								</span>
							{/if}
						</div>

						{#if !instanceState}
							<div class="px-4 py-6 text-center">
								<p class="text-xs text-muted-foreground">Loading state...</p>
							</div>
						{:else if hasTokens}
							<div class="divide-y divide-border">
								{#each Object.entries(markingTokens) as [placeId, tokens] (placeId)}
									{#if tokens.length > 0}
										<div>
											<button
												type="button"
												class="flex w-full items-center gap-2 px-4 py-2.5 text-left transition-colors hover:bg-accent/50"
												onclick={() => togglePlace(placeId)}
											>
												{#if expandedPlaces.has(placeId)}
													<ChevronDown class="size-3 text-muted-foreground" />
												{:else}
													<ChevronRight class="size-3 text-muted-foreground" />
												{/if}
												<CircleDot class="size-3 text-blue-500" />
												<span class="text-xs font-medium text-foreground">{placeId}</span>
												<span class="ml-auto text-[10px] text-muted-foreground">
													{tokens.length} token{tokens.length !== 1 ? 's' : ''}
												</span>
											</button>

											{#if expandedPlaces.has(placeId)}
												<div class="border-t border-border/50 bg-muted/30 px-4 py-2">
													{#each tokens as token, i (token.id)}
														<div class="flex items-start gap-2 py-1.5 {i > 0 ? 'border-t border-border/30' : ''}">
															<span class="shrink-0 rounded bg-muted px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground" title={token.id}>
																{token.id.slice(0, 8)}
															</span>
															{#if token.color.type === 'Unit'}
																<Badge variant="secondary" class="bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-400">
																	Unit
																</Badge>
															{:else if token.color.type === 'Integer'}
																<Badge variant="secondary" class="bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300">
																	{token.color.value}
																</Badge>
															{:else if token.color.type === 'Data'}
																<Badge variant="secondary" class="bg-violet-100 text-violet-700 dark:bg-violet-900 dark:text-violet-300">
																	Data
																</Badge>
															{/if}
														</div>
														{#if token.color.type === 'Data'}
															<pre class="mb-1 ml-6 mt-1 max-h-32 overflow-auto rounded bg-muted px-2 py-1.5 font-mono text-[10px] text-foreground">{JSON.stringify(token.color.value, null, 2)}</pre>
														{/if}
													{/each}
												</div>
											{/if}
										</div>
									{/if}
								{/each}
							</div>
						{:else}
							<div class="px-4 py-6 text-center">
								<CircleDot class="mx-auto size-6 text-muted-foreground/30" />
								<p class="mt-2 text-xs text-muted-foreground">
									{(instanceState.event_count ?? 0) === 0
										? 'No events — event log may have been purged'
										: 'No tokens in any place'}
								</p>
							</div>
						{/if}
					</div>

					<!-- Enabled Transitions (only when engine is hot) -->
					{#if instanceState?.engine?.available && instanceState.enabled_transitions.length > 0}
						<div class="rounded-lg border border-border bg-card">
							<div class="border-b border-border px-4 py-2.5">
								<span class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
									Enabled Transitions
								</span>
							</div>
							<div class="px-4 py-3">
								{#each instanceState.enabled_transitions as transitionId (transitionId)}
									<div class="mb-1 flex items-center gap-2">
										<Activity class="size-3 text-amber-500" />
										<span class="text-xs text-foreground">{transitionId}</span>
									</div>
								{/each}
							</div>
						</div>
					{/if}

					<!-- Event Log -->
					{#if instanceState?.events?.length}
						<div class="rounded-lg border border-border bg-card" data-testid="event-log-section">
							<button
								type="button"
								class="flex w-full items-center gap-2 px-4 py-2.5 text-left transition-colors hover:bg-accent/50"
								onclick={() => (showEventLog = !showEventLog)}
							>
								{#if showEventLog}
									<ChevronDown class="size-3 text-muted-foreground" />
								{:else}
									<ChevronRight class="size-3 text-muted-foreground" />
								{/if}
								<span class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
									Event Log
								</span>
								<span class="text-[10px] text-muted-foreground">
									{instanceState.event_count} events
								</span>
							</button>

							{#if showEventLog}
								<div class="max-h-96 divide-y divide-border/50 overflow-y-auto border-t border-border">
									{#each instanceState.events as event (event.sequence)}
										<div>
											<button
												type="button"
												class="flex w-full items-center gap-2 px-4 py-1.5 text-left transition-colors hover:bg-accent/30"
												onclick={() => toggleEvent(event.sequence)}
											>
												<span class="shrink-0 w-6 text-right font-mono text-[10px] text-muted-foreground">
													{event.sequence}
												</span>
												<Zap class="size-2.5 shrink-0 {eventTypeColors[event.event.type] ?? 'text-gray-500'}" />
												<span class="text-[11px] font-medium {eventTypeColors[event.event.type] ?? 'text-gray-500'}">
													{event.event.type}
												</span>
												<span class="truncate text-[10px] text-muted-foreground">
													{eventSummary(event)}
												</span>
												<span class="ml-auto shrink-0 text-[9px] text-muted-foreground/60">
													{new Date(event.timestamp).toLocaleTimeString()}
												</span>
											</button>
											{#if expandedEvents.has(event.sequence)}
												<pre class="mx-4 mb-2 max-h-48 overflow-auto rounded bg-muted px-3 py-2 font-mono text-[10px] text-foreground">{JSON.stringify(event.event, null, 2)}</pre>
											{/if}
										</div>
									{/each}
								</div>
							{/if}
						</div>
					{/if}
				{/if}
			</div>
		{/if}
	</div>
</div>
