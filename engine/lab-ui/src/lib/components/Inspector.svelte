<script lang="ts">
	import { multiNetStore } from '$lib/stores/multi-net.svelte';
	import { Pencil, ExternalLink } from '@lucide/svelte';
	import type { SelectedElement } from '$lib/stores/lab.svelte';
	import { Separator } from '$lib/components/ui/separator';
	import { CopyButton } from '$lib/components/ui/copy-button';

	interface Props {
		onOpenScript?: () => void;
		onViewToken?: () => void;
		onNavigateToChild?: (netId: string) => void;
	}

	let { onOpenScript, onViewToken, onNavigateToChild }: Props = $props();

	const store = $derived(multiNetStore.activeStore);

	let injectJsonInput = $state('{}');
	let injectError = $state<string | null>(null);
	let injectSuccess = $state(false);
	let cancelInProgress = $state(false);
	let cancelSuccess = $state(false);
	let cancelError = $state<string | null>(null);

	// Track previous selection for back navigation
	let previousSelection = $state<SelectedElement>(null);

	// When selection changes, track the previous non-token selection
	$effect(() => {
		const current = store?.selectedElement ?? null;
		// When we navigate TO a token, save where we came from
		// (but don't overwrite if we're already on a token — that means we're browsing tokens)
		if (current?.type === 'token') return;
		previousSelection = current;
	});

	// Get details based on selection type
	const placeDetails = $derived(store?.getSelectedPlaceDetails() ?? null);
	const transitionDetails = $derived(store?.getSelectedTransitionDetails() ?? null);
	const tokenDetails = $derived(store?.getSelectedTokenDetails() ?? null);
	const eventDetails = $derived(store?.getSelectedEventDetails() ?? null);
	const groupDetails = $derived(store?.getSelectedGroupDetails() ?? null);

	// Detect if a token is a "lease" (has job_id and worker_id)
	function isLeaseToken(token: { color: { type: string; value?: unknown } }): boolean {
		if (token.color.type !== 'Data' || !token.color.value) return false;
		const data = token.color.value as Record<string, unknown>;
		return 'job_id' in data && 'worker_id' in data;
	}

	// Detect if a token has external coordination provenance
	function hasCoordinationProvenance(token: { color: { type: string; value?: unknown } }): boolean {
		if (token.color.type !== 'Data' || !token.color.value) return false;
		const data = token.color.value as Record<string, unknown>;
		return '_provenance' in data && typeof data._provenance === 'object';
	}

	// Get coordination provenance from token
	function getCoordinationProvenance(token: { color: { type: string; value?: unknown } }): {
		source: string;
		signal_type: string;
		workflow_id: string;
		adapter_pool: string;
		request_sent_at?: string;
		response_received_at?: string;
		confirm_sent_at?: string;
		transition?: string;
	} | null {
		if (!hasCoordinationProvenance(token)) return null;
		const data = token.color.value as Record<string, unknown>;
		return data._provenance as any;
	}

	// Format duration between two ISO timestamps
	function formatDuration(start: string, end: string): string {
		const startMs = new Date(start).getTime();
		const endMs = new Date(end).getTime();
		const durationMs = endMs - startMs;
		if (durationMs < 1000) return `${durationMs}ms`;
		return `${(durationMs / 1000).toFixed(2)}s`;
	}

	// Get signal type badge color
	function getSignalTypeBadgeClass(signalType: string): string {
		switch (signalType) {
			case 'accepted': return 'bg-green-500/15 text-green-500';
			case 'denied': return 'bg-red-500/15 text-red-500';
			case 'confirmed': return 'bg-blue-500/15 text-blue-500';
			case 'failed': return 'bg-red-500/15 text-red-500';
			default: return 'bg-muted text-foreground';
		}
	}

	// Get job_id from lease token
	function getLeaseJobId(token: { color: { type: string; value?: unknown } }): string | null {
		if (!isLeaseToken(token)) return null;
		const data = token.color.value as Record<string, unknown>;
		return data.job_id as string;
	}

	// Handle cancel action - inject a cancel signal
	async function handleSimulateCancel() {
		if (!tokenDetails || !isLeaseToken(tokenDetails.token)) return;
		const jobId = getLeaseJobId(tokenDetails.token);
		if (!jobId) return;

		cancelInProgress = true;
		cancelError = null;
		cancelSuccess = false;

		try {
			const result = await store?.injectToken('p_sig_cancel', { correlation_id: jobId });
			if (!result) { cancelError = 'No response'; return; }
			if (result.success) {
				cancelSuccess = true;
				setTimeout(() => (cancelSuccess = false), 2000);
			} else {
				cancelError = result.error ?? 'Failed to inject cancel signal';
			}
		} catch (e) {
			cancelError = e instanceof Error ? e.message : 'Unknown error';
		} finally {
			cancelInProgress = false;
		}
	}

	// Format guard for display
	function formatGuard(guard: any): string {
		if (!guard || guard.type === 'Always') return 'Always (no guard)';
		switch (guard.type) {
			case 'IntegerGreaterThan':
				return `Integer > ${guard.value}`;
			case 'IntegerLessThan':
				return `Integer < ${guard.value}`;
			case 'DataHasField':
				return `Has field "${guard.field}"`;
			case 'FieldCompare':
				return `${guard.field} ${guard.op} ${JSON.stringify(guard.value)}`;
			case 'ColorEquals':
				return `Color equals ${JSON.stringify(guard.value)}`;
			default:
				return guard.type;
		}
	}

	// Format JSON with syntax highlighting classes
	function formatJson(value: unknown): string {
		return JSON.stringify(value, null, 2);
	}

	// Handle token injection
	async function handleInjectToken() {
		if (!placeDetails) return;
		injectError = null;
		injectSuccess = false;

		try {
			const data = JSON.parse(injectJsonInput);
			const result = await store?.injectToken(placeDetails.place.id, data);
			if (!result) { injectError = 'No response'; return; }
			if (result.success) {
				injectSuccess = true;
				injectJsonInput = '{}';
				setTimeout(() => (injectSuccess = false), 2000);
			} else {
				injectError = result.error ?? 'Failed to inject token';
			}
		} catch (e) {
			injectError = e instanceof Error ? e.message : 'Invalid JSON';
		}
	}

	// Navigate to event in timeline and select it for inspection
	function goToEvent(sequence: number) {
		const idx = store?.events.findIndex((e) => e.sequence === sequence);
		if (idx != null && idx >= 0) {
			store?.setReplayIndex(idx);
		}
		store?.selectEvent(sequence);
	}
</script>

<div class="inspector h-full bg-card border-l border-border flex flex-col">
	<!-- Header -->
	<div class="px-3 py-2 border-b border-border bg-muted flex items-center justify-between">
		<h3 class="font-semibold text-foreground text-sm">Inspector</h3>
		{#if store?.selectedElement}
			<button
				onclick={() => store?.clearSelection()}
				class="text-muted-foreground hover:text-foreground text-sm"
			>
				Clear
			</button>
		{/if}
	</div>

	<!-- Content -->
	<div class="flex-1 overflow-y-auto p-4">
		{#if !store?.selectedElement}
			<div class="text-muted-foreground text-sm text-center py-8">
				<p>Click on a place or transition to inspect it</p>
			</div>
		{:else if store?.selectedElement.type === 'place' && placeDetails}
			<!-- Place Inspector -->
			<div class="space-y-4">
				<div class="bg-muted/50 rounded-lg p-3">
					<h3 class="text-lg font-medium text-foreground">{placeDetails.place.name}</h3>
					<p class="text-xs text-muted-foreground font-mono">{placeDetails.place.id}</p>
					<div class="flex items-center gap-2 mt-2">
						<span
							class="px-2 py-0.5 text-xs rounded {(placeDetails.place as any).kind === 'signal'
								? 'bg-amber-500/15 text-amber-500'
								: (placeDetails.place as any).kind === 'bridge_out'
									? 'bg-rose-500/15 text-rose-500'
									: (placeDetails.place as any).kind === 'bridge_reply'
										? 'bg-indigo-500/15 text-indigo-500'
										: (placeDetails.place as any).kind === 'bridge_in'
											? 'bg-teal-500/15 text-teal-500'
											: 'bg-blue-500/15 text-blue-500'}"
						>
							{(placeDetails.place as any).kind ?? 'internal'}
						</span>
						{#if placeDetails.place.capacity}
							<span class="text-xs text-muted-foreground">
								Capacity: <span class="font-medium">{placeDetails.place.capacity}</span>
							</span>
						{/if}
					</div>
				</div>

				<Separator />

				<!-- Tokens List -->
				<div class="bg-muted/50 rounded-lg p-3">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
						Tokens ({placeDetails.tokens.length})
					</h4>
					{#if placeDetails.tokens.length === 0}
						<p class="text-sm text-muted-foreground italic">No tokens</p>
					{:else}
						<div class="space-y-2 max-h-48 overflow-y-auto">
							{#each placeDetails.tokens as token}
								<button
									class="w-full text-left p-2 rounded border transition-colors {isLeaseToken(token)
										? 'border-l-2 border-l-amber-500 border-amber-500/30 bg-amber-500/10 hover:border-amber-500/50 hover:bg-amber-500/20'
										: 'border-l-2 border-l-primary/50 border-border hover:border-primary/50 hover:bg-primary/10'}"
									onclick={() => store?.selectToken(placeDetails.place.id, token.id)}
								>
									<div class="flex items-start gap-2">
										<span class="text-xs px-1.5 py-0.5 rounded bg-muted text-muted-foreground font-medium shrink-0">
											{token.color.type}
										</span>
										{#if isLeaseToken(token)}
											<span class="text-xs px-1.5 py-0.5 rounded bg-amber-500/20 text-amber-400 font-medium shrink-0">
												Lease
											</span>
										{/if}
										<div class="flex-1 min-w-0">
											{#if token.color.type === 'Unit'}
												<span class="text-sm text-muted-foreground italic">empty</span>
											{:else if token.color.type === 'Integer'}
												<span class="text-sm font-mono text-primary font-medium">{token.color.value}</span>
											{:else if token.color.type === 'Data'}
												<pre class="text-xs text-foreground/80 truncate">{JSON.stringify(token.color.value)}</pre>
											{/if}
										</div>
									</div>
									<div class="text-[10px] font-mono text-muted-foreground mt-1 truncate">{token.id.slice(0, 8)}...</div>
								</button>
							{/each}
						</div>
					{/if}
				</div>

				<Separator />

				<!-- Token Injection -->
				<div>
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">Inject Token</h4>
					<textarea
						bind:value={injectJsonInput}
						placeholder={'{"amount": 500}'}
						class="w-full h-20 text-sm font-mono p-2 border rounded bg-gray-900 text-green-400 resize-none"
						spellcheck="false"
					></textarea>
					{#if injectError}
						<p class="text-xs text-red-600 mt-1">{injectError}</p>
					{/if}
					{#if injectSuccess}
						<p class="text-xs text-green-600 mt-1">Token injected!</p>
					{/if}
					<button
						onclick={handleInjectToken}
						disabled={store?.loading}
						class="mt-2 w-full px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50"
					>
						{store?.loading ? 'Injecting...' : 'Inject Token'}
					</button>
				</div>
			</div>
		{:else if store?.selectedElement.type === 'transition' && transitionDetails}
			<!-- Transition Inspector -->
			<div class="space-y-4">
				<div class="bg-muted/50 rounded-lg p-3">
					<h3 class="text-lg font-medium text-foreground">{transitionDetails.transition.name}</h3>
					<p class="text-xs text-muted-foreground font-mono">{transitionDetails.transition.id}</p>
					<div class="flex items-center gap-2 mt-2">
						{#if (transitionDetails.transition as any).effect_handler_id}
							<span class="px-2 py-0.5 text-xs rounded font-medium bg-purple-500/15 text-purple-700 dark:text-purple-400">
								Effect
							</span>
							<span class="text-xs font-mono text-muted-foreground">
								{(transitionDetails.transition as any).effect_handler_id}
							</span>
						{:else}
							<span class="px-2 py-0.5 text-xs rounded font-medium bg-blue-500/15 text-blue-700 dark:text-blue-400">
								Rhai Script
							</span>
						{/if}
					</div>
				</div>

				<Separator />

				<!-- Effect Handler -->
				{#if (transitionDetails.transition as any).effect_handler_id}
					<div class="bg-muted/50 rounded-lg p-3">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">Effect Handler</h4>
						<div class="px-3 py-2 rounded text-sm bg-purple-500/10 border border-purple-500/30 text-purple-700 dark:text-purple-400 font-mono">
							{(transitionDetails.transition as any).effect_handler_id}
						</div>
						<p class="text-xs text-muted-foreground mt-2">
							Runs a registered side-effect handler instead of a Rhai script.
						</p>
					</div>

					<Separator />
				{/if}

				<!-- Guard -->
				{#if true}
					{@const guardScript = (transitionDetails.transition as any).guard as string | null}
					<div class="bg-muted/50 rounded-lg p-3">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">Guard Condition</h4>
						<div
							class="px-3 py-2 rounded text-sm font-mono {guardScript
								? 'bg-amber-500/10 border border-amber-500/30 text-amber-700 dark:text-amber-400'
								: 'bg-muted text-muted-foreground'}"
						>
							{guardScript ?? 'None (always enabled)'}
						</div>
					</div>
				{/if}

				<Separator />

				<!-- Input Places -->
				<div class="bg-muted/50 rounded-lg p-3">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
						Input Places ({transitionDetails.inputArcs.length})
					</h4>
					{#if transitionDetails.inputArcs.length === 0}
						<p class="text-sm text-muted-foreground italic">None</p>
					{:else}
						<ul class="space-y-1">
							{#each transitionDetails.inputArcs as arc}
								<li>
									<button
										class="text-sm text-primary hover:underline"
										onclick={() => store?.selectPlace(arc.place_id)}
									>
										{store?.getPlaceName(arc.place_id)}
									</button>
									{#if arc.weight && arc.weight > 1}
										<span class="text-xs text-muted-foreground">(weight: {arc.weight})</span>
									{/if}
								</li>
							{/each}
						</ul>
					{/if}
				</div>

				<!-- Output Places -->
				<div class="bg-muted/50 rounded-lg p-3">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
						Output Places ({transitionDetails.outputArcs.length})
					</h4>
					{#if transitionDetails.outputArcs.length === 0}
						<p class="text-sm text-muted-foreground italic">None</p>
					{:else}
						<ul class="space-y-1">
							{#each transitionDetails.outputArcs as arc}
								<li>
									<button
										class="text-sm text-primary hover:underline"
										onclick={() => store?.selectPlace(arc.place_id)}
									>
										{store?.getPlaceName(arc.place_id)}
									</button>
									{#if arc.weight && arc.weight > 1}
										<span class="text-xs text-muted-foreground">(weight: {arc.weight})</span>
									{/if}
								</li>
							{/each}
						</ul>
					{/if}
				</div>

				<!-- Open Script/Effect Sheet -->
				{#if onOpenScript}
					<button
						onclick={onOpenScript}
						class="w-full flex items-center justify-center gap-2 px-3 py-2 text-sm font-medium rounded bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
					>
						<Pencil class="w-4 h-4" />
						View / Edit Logic
					</button>
				{/if}
			</div>
		{:else if store?.selectedElement.type === 'token' && tokenDetails}
			<!-- Token Inspector -->
			<div class="space-y-4">
				<div class="bg-muted/50 rounded-lg p-3">
					<button
						class="text-sm text-primary hover:underline mb-2"
						onclick={() => {
							if (previousSelection?.type === 'event') {
								store?.selectEvent(previousSelection.sequence);
							} else if (store?.selectedElement?.type === 'token') {
								store?.selectPlace(store?.selectedElement.placeId);
							}
						}}
					>
						&larr; {previousSelection?.type === 'event' ? `Back to Event #${previousSelection.sequence}` : `Back to ${tokenDetails.placeName}`}
					</button>
					<h3 class="text-lg font-medium text-foreground">Token</h3>
					<p class="text-xs text-muted-foreground font-mono">{tokenDetails.token.id}</p>
				</div>

				<div class="flex items-center gap-2">
					{#if tokenDetails.token.color.type !== 'Unit'}
						<CopyButton text={tokenDetails.token.color.type === 'Integer' ? String(tokenDetails.token.color.value) : JSON.stringify(tokenDetails.token.color.value, null, 2)} />
					{/if}
					{#if onViewToken}
						<button
							onclick={onViewToken}
							class="flex-1 flex items-center justify-center gap-2 px-3 py-2 text-sm font-medium rounded bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
						>
							View Details
						</button>
					{/if}
				</div>

			</div>
		{:else if store?.selectedElement.type === 'event' && eventDetails}
			<!-- Event Inspector -->
			<div class="space-y-4">
				<div class="bg-muted/50 rounded-lg p-3">
					<h3 class="text-lg font-medium text-foreground">Event #{eventDetails.event.sequence}</h3>
					<p class="text-xs text-muted-foreground">
						{new Date(eventDetails.event.timestamp).toLocaleString()}
					</p>
				</div>

				<!-- Event Type Badge -->
				<div>
					<span
						class="px-2 py-1 text-xs rounded font-medium
						{eventDetails.eventTypeName === 'TransitionFired'
							? 'bg-green-500/15 text-green-500'
							: eventDetails.eventTypeName === 'EffectCompleted'
								? 'bg-emerald-500/15 text-emerald-500'
								: eventDetails.eventTypeName === 'EffectFailed'
									? 'bg-red-500/15 text-red-500'
									: eventDetails.eventTypeName === 'TokenCreated'
										? 'bg-blue-500/15 text-blue-500'
										: eventDetails.eventTypeName === 'TokenBridgedOut'
											? 'bg-rose-500/15 text-rose-500'
											: eventDetails.eventTypeName === 'ErrorOccurred'
												? 'bg-red-500/15 text-red-500'
												: 'bg-muted text-foreground'}"
					>
						{eventDetails.eventTypeName}
					</span>
				</div>

				{#if eventDetails.eventTypeName === 'TransitionFired'}
					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Transition</h4>
						<button
							class="text-sm text-primary hover:underline"
							onclick={() => {
								const e = eventDetails.event.event as any;
								store?.selectTransition(e.transition_id);
							}}
						>
							{eventDetails.transitionName}
						</button>
					</div>

					{#if eventDetails.consumedTokens && eventDetails.consumedTokens.length > 0}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Consumed ({eventDetails.consumedTokens.length})</h4>
							<div class="space-y-0.5">
								{#each eventDetails.consumedTokens as ct}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-red-500">-</span>
										<button class="text-primary hover:underline" onclick={() => store?.selectPlace(ct.placeId)}>{ct.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { store?.selectToken(ct.placeId, ct.tokenId); onViewToken?.(); }}
										>{ct.tokenId.slice(0, 8)}</button>
									</div>
								{/each}
							</div>
						</div>
					{/if}

					{#if eventDetails.producedTokens && eventDetails.producedTokens.length > 0}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Produced ({eventDetails.producedTokens.length})</h4>
							<div class="space-y-0.5">
								{#each eventDetails.producedTokens as pt}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-green-500">+</span>
										<button class="text-primary hover:underline" onclick={() => store?.selectPlace(pt.placeId)}>{pt.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { store?.selectToken(pt.placeId, pt.token.id); onViewToken?.(); }}
										>{pt.token.id.slice(0, 8)}</button>
									</div>
								{/each}
							</div>
						</div>
					{/if}

				{:else if eventDetails.eventTypeName === 'EffectCompleted'}
					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Transition</h4>
						<button
							class="text-sm text-primary hover:underline"
							onclick={() => {
								const e = eventDetails.event.event as any;
								store?.selectTransition(e.transition_id);
							}}
						>
							{eventDetails.transitionName}
						</button>
					</div>

					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Handler</h4>
						<span class="px-2 py-0.5 text-xs rounded bg-emerald-500/15 text-emerald-500 font-mono">
							{eventDetails.effectHandlerId}
						</span>
					</div>

					{#if eventDetails.consumedTokens && eventDetails.consumedTokens.length > 0}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Consumed ({eventDetails.consumedTokens.length})</h4>
							<div class="space-y-0.5">
								{#each eventDetails.consumedTokens as ct}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-red-500">-</span>
										<button class="text-primary hover:underline" onclick={() => store?.selectPlace(ct.placeId)}>{ct.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { store?.selectToken(ct.placeId, ct.tokenId); onViewToken?.(); }}
										>{ct.tokenId.slice(0, 8)}</button>
									</div>
								{/each}
							</div>
						</div>
					{/if}

					{#if eventDetails.producedTokens && eventDetails.producedTokens.length > 0}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Produced ({eventDetails.producedTokens.length})</h4>
							<div class="space-y-0.5">
								{#each eventDetails.producedTokens as pt}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-green-500">+</span>
										<button class="text-primary hover:underline" onclick={() => store?.selectPlace(pt.placeId)}>{pt.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { store?.selectToken(pt.placeId, pt.token.id); onViewToken?.(); }}
										>{pt.token.id.slice(0, 8)}</button>
									</div>
								{/each}
							</div>
						</div>
					{/if}

				{:else if eventDetails.eventTypeName === 'EffectFailed'}
					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Transition</h4>
						<button
							class="text-sm text-primary hover:underline"
							onclick={() => {
								const e = eventDetails.event.event as any;
								store?.selectTransition(e.transition_id);
							}}
						>
							{eventDetails.transitionName}
						</button>
					</div>

					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Handler</h4>
						<span class="px-2 py-0.5 text-xs rounded bg-red-500/15 text-red-500 font-mono">
							{eventDetails.effectHandlerId}
						</span>
					</div>

					<div class="bg-red-500/10 border border-red-500/30 rounded p-2 text-xs text-red-400">
						{eventDetails.errorMessage}
					</div>

					{#if eventDetails.consumedTokens && eventDetails.consumedTokens.length > 0}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Consumed ({eventDetails.consumedTokens.length})</h4>
							<div class="space-y-0.5">
								{#each eventDetails.consumedTokens as ct}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-red-500">-</span>
										<button class="text-primary hover:underline" onclick={() => store?.selectPlace(ct.placeId)}>{ct.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { store?.selectToken(ct.placeId, ct.tokenId); onViewToken?.(); }}
										>{ct.tokenId.slice(0, 8)}</button>
									</div>
								{/each}
							</div>
						</div>
					{/if}

					{#if eventDetails.producedTokens && eventDetails.producedTokens.length > 0}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Produced ({eventDetails.producedTokens.length})</h4>
							<div class="space-y-0.5">
								{#each eventDetails.producedTokens as pt}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-green-500">+</span>
										<button class="text-primary hover:underline" onclick={() => store?.selectPlace(pt.placeId)}>{pt.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { store?.selectToken(pt.placeId, pt.token.id); onViewToken?.(); }}
										>{pt.token.id.slice(0, 8)}</button>
									</div>
								{/each}
							</div>
						</div>
					{/if}

				{:else if eventDetails.eventTypeName === 'TokenCreated' && eventDetails.token}
					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Place</h4>
						<button
							class="text-sm text-primary hover:underline"
							onclick={() => {
								const e = eventDetails.event.event as any;
								store?.selectPlace(e.place_id);
							}}
						>
							{eventDetails.placeName}
						</button>
					</div>

					<div class="flex items-center gap-2">
						<span class="text-xs text-muted-foreground font-mono">{eventDetails.token.id.slice(0, 8)}</span>
						<button
							onclick={() => { store?.selectToken((eventDetails.event.event as any).place_id, eventDetails.token!.id); onViewToken?.(); }}
							class="px-3 py-1.5 text-xs font-medium rounded bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
						>
							View Details
						</button>
					</div>

				{:else if eventDetails.eventTypeName === 'TokenConsumed'}
					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">From Place</h4>
						<button
							class="text-sm text-primary hover:underline"
							onclick={() => {
								const e = eventDetails.event.event as any;
								store?.selectPlace(e.place_id);
							}}
						>
							{eventDetails.placeName}
						</button>
					</div>

				{:else if eventDetails.eventTypeName === 'TokenBridgedOut'}
					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Transition</h4>
						<button
							class="text-sm text-primary hover:underline"
							onclick={() => {
								const e = eventDetails.event.event as any;
								store?.selectTransition(e.transition_id);
							}}
						>
							{eventDetails.transitionName}
						</button>
					</div>

					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Source</h4>
						<button
							class="text-sm text-primary hover:underline"
							onclick={() => {
								const e = eventDetails.event.event as any;
								store?.selectPlace(e.source_place_id);
							}}
						>
							{eventDetails.placeName}
						</button>
					</div>

					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Target</h4>
						<span class="px-2 py-0.5 text-xs rounded font-medium bg-rose-500/15 text-rose-500">
							{eventDetails.targetNetId} / {eventDetails.targetPlaceName}
						</span>
					</div>

					{#if eventDetails.token}
						<div class="flex items-center gap-2">
							<span class="text-xs text-muted-foreground font-mono">{eventDetails.token.id.slice(0, 8)}</span>
							<button
								onclick={() => { store?.selectToken((eventDetails.event.event as any).source_place_id, eventDetails.token!.id); onViewToken?.(); }}
								class="px-3 py-1.5 text-xs font-medium rounded bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
							>
								View Details
							</button>
						</div>
					{/if}

				{:else if eventDetails.eventTypeName === 'ErrorOccurred'}
					<div class="bg-red-500/10 border border-red-500/30 rounded p-2 text-xs text-red-400">
						{eventDetails.errorMessage}
					</div>

				{:else if eventDetails.eventTypeName === 'NetInitialized'}
					<div class="text-sm text-muted-foreground">
						Net initialized with its topology and tokens.
					</div>
				{/if}

				<Separator />

				<!-- Hash Chain -->
				<div class="bg-muted/50 rounded-lg p-3">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">Hash Chain</h4>
					<div class="text-xs font-mono space-y-1">
						<div>
							<span class="text-muted-foreground">Hash:</span>
							<span class="text-foreground/80 break-all">{eventDetails.event.hash}</span>
						</div>
						{#if eventDetails.event.previous_hash}
							<div>
								<span class="text-muted-foreground">Prev:</span>
								<span class="text-foreground/80 break-all">{eventDetails.event.previous_hash}</span>
							</div>
						{/if}
					</div>
				</div>
			</div>
		{:else if store?.selectedElement.type === 'group' && groupDetails}
			<!-- Group Inspector (collapsed meta-node) -->
			<div class="space-y-4">
				<div class="bg-muted/50 rounded-lg p-3">
					<h3 class="text-lg font-medium text-foreground">{groupDetails.group.name}</h3>
					<p class="text-xs text-muted-foreground font-mono">{groupDetails.group.id}</p>
					<div class="flex items-center gap-2 mt-2">
						<span class="px-2 py-0.5 text-xs rounded bg-primary/15 text-primary font-medium">
							Group
						</span>
						<span class="text-xs text-muted-foreground">
							{groupDetails.places.length} places · {groupDetails.transitions.length} transitions
						</span>
					</div>
				</div>

				{#if groupDetails.childGroups.length > 0}
					<Separator />
					<div class="bg-muted/50 rounded-lg p-3">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
							Sub-groups ({groupDetails.childGroups.length})
						</h4>
						<div class="space-y-1">
							{#each groupDetails.childGroups as child}
								<button
									class="w-full text-left px-2 py-1 rounded border border-border hover:border-primary/50 hover:bg-primary/10 transition-colors"
									onclick={() => store?.selectGroup(child.id)}
								>
									<span class="text-xs font-medium text-foreground">{child.name}</span>
								</button>
							{/each}
						</div>
					</div>
				{/if}

				<Separator />

				<!-- Places in group -->
				<div class="bg-muted/50 rounded-lg p-3">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
						Places ({groupDetails.places.length})
					</h4>
					<div class="space-y-1 max-h-40 overflow-y-auto">
						{#each groupDetails.places as place}
							{@const count = groupDetails.allTokens.filter(t => t.placeId === place.id).length}
							<button
								class="w-full text-left px-2 py-1 rounded border border-border hover:border-primary/50 hover:bg-primary/10 transition-colors flex items-center gap-2"
								onclick={() => store?.selectPlace(place.id)}
							>
								<span class="text-xs font-medium text-foreground truncate">{place.name}</span>
								{#if count > 0}
									<span class="ml-auto text-[10px] font-mono px-1.5 py-0.5 rounded-full bg-primary/15 text-primary shrink-0">
										{count}
									</span>
								{/if}
							</button>
						{/each}
					</div>
				</div>

				<Separator />

				<!-- Tokens across all places -->
				<div class="bg-muted/50 rounded-lg p-3">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
						Tokens ({groupDetails.allTokens.length})
					</h4>
					{#if groupDetails.allTokens.length === 0}
						<p class="text-sm text-muted-foreground italic">No tokens in this group</p>
					{:else}
						<div class="space-y-2 max-h-64 overflow-y-auto">
							{#each groupDetails.allTokens as { placeId, placeName, token }}
								<button
									class="w-full text-left p-2 rounded border border-l-2 border-l-primary/50 border-border hover:border-primary/50 hover:bg-primary/10 transition-colors"
									onclick={() => store?.selectToken(placeId, token.id)}
								>
									<div class="flex items-start gap-2">
										<span class="text-xs px-1.5 py-0.5 rounded bg-muted text-muted-foreground font-medium shrink-0">
											{token.color.type}
										</span>
										<div class="flex-1 min-w-0">
											{#if token.color.type === 'Unit'}
												<span class="text-sm text-muted-foreground italic">empty</span>
											{:else if token.color.type === 'Data'}
												<pre class="text-xs text-foreground/80 truncate">{JSON.stringify(token.color.value)}</pre>
											{:else}
												<span class="text-sm font-mono text-primary">{token.color.value}</span>
											{/if}
										</div>
									</div>
									<div class="text-[10px] font-mono text-muted-foreground mt-1">
										<span class="text-primary/60">{placeName}</span> · {token.id.slice(0, 8)}...
									</div>
								</button>
							{/each}
						</div>
					{/if}
				</div>

				<Separator />

				<!-- Transitions in group -->
				<div class="bg-muted/50 rounded-lg p-3">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
						Transitions ({groupDetails.transitions.length})
					</h4>
					<div class="space-y-1 max-h-40 overflow-y-auto">
						{#each groupDetails.transitions as transition}
							<button
								class="w-full text-left px-2 py-1 rounded border border-border hover:border-primary/50 hover:bg-primary/10 transition-colors flex items-center gap-2"
								onclick={() => store?.selectTransition(transition.id)}
							>
								<span class="text-xs font-medium text-foreground truncate">{transition.name}</span>
								{#if (transition as any).effect_handler_id}
									<span class="ml-auto text-[10px] px-1 py-0.5 rounded bg-purple-500/15 text-purple-500 font-mono shrink-0">FX</span>
								{/if}
							</button>
						{/each}
					</div>
				</div>
			</div>
		{:else if store?.selectedElement.type === 'remotenet'}
			{@const rn = store.selectedElement}
			<!-- Remote Net Inspector -->
			<div class="space-y-4">
				<div class="bg-muted/50 rounded-lg p-3">
					<h3 class="text-lg font-medium text-foreground">{rn.label}</h3>
					<p class="text-xs text-muted-foreground font-mono">{rn.id}</p>
					<div class="flex items-center gap-2 mt-2">
						<span class="px-2 py-0.5 text-xs rounded bg-teal-500/15 text-teal-500 font-medium">
							Remote Net
						</span>
						{#if rn.childNetIds.length > 0}
							<span class="text-xs text-muted-foreground">
								{rn.childNetIds.length} {rn.childNetIds.length === 1 ? 'instance' : 'instances'}
							</span>
						{/if}
					</div>
				</div>

				<Separator />

				<!-- Bridge Ports -->
				{#if rn.targets.length > 0}
					<div class="bg-muted/50 rounded-lg p-3">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
							Outbound Ports ({rn.targets.length})
						</h4>
						<div class="space-y-1">
							{#each rn.targets as port}
								<div class="px-2 py-1 rounded border border-border flex items-center gap-2">
									<span class="w-2 h-2 rounded-full bg-rose-400 shrink-0"></span>
									<span class="text-xs font-mono text-foreground truncate">{port}</span>
								</div>
							{/each}
						</div>
					</div>
				{/if}

				{#if rn.sources.length > 0}
					<div class="bg-muted/50 rounded-lg p-3">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
							Inbound Ports ({rn.sources.length})
						</h4>
						<div class="space-y-1">
							{#each rn.sources as port}
								<div class="px-2 py-1 rounded border border-border flex items-center gap-2">
									<span class="w-2 h-2 rounded-full bg-teal-400 shrink-0"></span>
									<span class="text-xs font-mono text-foreground truncate">{port}</span>
								</div>
							{/each}
						</div>
					</div>
				{/if}

				{#if rn.childNetIds.length > 0}
					<Separator />

					<!-- Child Net Instances -->
					<div class="bg-muted/50 rounded-lg p-3">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
							Child Instances ({rn.childNetIds.length})
						</h4>
						<div class="space-y-1 max-h-48 overflow-y-auto">
							{#each rn.childNetIds as childId}
								<button
									class="w-full text-left px-2 py-1.5 rounded border border-border hover:border-teal-500/50 hover:bg-teal-500/10 transition-colors flex items-center gap-2"
									onclick={() => onNavigateToChild?.(childId)}
								>
									<span class="text-xs font-mono text-foreground truncate">{childId.slice(0, 12)}...</span>
									<ExternalLink class="w-3 h-3 ml-auto shrink-0 text-teal-500" />
								</button>
							{/each}
						</div>
					</div>
				{:else}
					<Separator />
					<div class="text-sm text-muted-foreground italic text-center py-2">
						No child instances spawned yet
					</div>
				{/if}
			</div>
		{/if}
	</div>
</div>
