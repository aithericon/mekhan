<script lang="ts">
	import { Pencil, ExternalLink } from '@lucide/svelte';
	import type { SelectedElement, Token, PersistedEvent, Port } from '$lib/types/petri';
	import { Separator } from '$lib/components/ui/separator';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import { Card } from '$lib/components/ui/card';
	import { Badge } from '$lib/components/ui/badge';
	import NodeKindBadge from './NodeKindBadge.svelte';

	// ---------------------------------------------------------------------------
	// Detail shape interfaces
	// ---------------------------------------------------------------------------

	interface PlaceDetails {
		place: { id: string; name: string; kind?: string; capacity?: number | null };
		tokens: Token[];
	}

	interface TransitionDetails {
		transition: {
			id: string;
			name: string;
			guard?: string | null;
			script?: string;
			effect_handler_id?: string | null;
			logic_type?: string;
			input_ports?: Port[];
			output_ports?: Port[];
		};
		inputArcs: { place_id: string; place_name?: string; weight?: number }[];
		outputArcs: { place_id: string; place_name?: string; weight?: number }[];
	}

	interface TokenDetails {
		token: Token;
		placeName: string;
		creationEvent?: PersistedEvent;
	}

	// ---------------------------------------------------------------------------
	// Props
	// ---------------------------------------------------------------------------

	interface Props {
		// Selection state — passed in from the parent / store projection
		selectedElement: SelectedElement;

		// Detail objects — pre-computed by the parent from the active store
		placeDetails?: PlaceDetails | null;
		transitionDetails?: TransitionDetails | null;
		tokenDetails?: TokenDetails | null;
		eventDetails?: any | null;
		groupDetails?: any | null;

		// Whether an operation is in flight (used to disable the inject button)
		loading?: boolean;

		// Name-resolution helpers
		getTransitionName?: (id: string) => string;
		getPlaceName?: (id: string) => string;

		// Selection callbacks — allow the Inspector to drive selection changes
		onSelectPlace?: (id: string) => void;
		onSelectTransition?: (id: string) => void;
		onSelectToken?: (placeId: string, tokenId: string) => void;
		onSelectGroup?: (id: string) => void;
		onSelectEvent?: (sequence: number) => void;
		onClearSelection?: () => void;

		// Action callbacks
		onInjectToken?: (placeId: string, data: unknown) => Promise<{ success: boolean; error?: string } | undefined>;
		onSetReplayIndex?: (index: number) => void;

		// Navigation / sheet callbacks
		onOpenScript?: () => void;
		onViewToken?: () => void;
		onNavigateToChild?: (netId: string) => void;
	}

	let {
		selectedElement,
		placeDetails = null,
		transitionDetails = null,
		tokenDetails = null,
		eventDetails = null,
		groupDetails = null,
		loading = false,
		getTransitionName,
		getPlaceName,
		onSelectPlace,
		onSelectTransition,
		onSelectToken,
		onSelectGroup,
		onSelectEvent,
		onClearSelection,
		onInjectToken,
		onSetReplayIndex,
		onOpenScript,
		onViewToken,
		onNavigateToChild,
	}: Props = $props();

	// ---------------------------------------------------------------------------
	// Local UI state
	// ---------------------------------------------------------------------------

	let injectJsonInput = $state('{}');
	let injectError = $state<string | null>(null);
	let injectSuccess = $state(false);
	let cancelInProgress = $state(false);
	let cancelSuccess = $state(false);
	let cancelError = $state<string | null>(null);

	// Track previous selection for back-navigation from token view.
	// We use a backing $state variable and only update it when the selection
	// moves to a non-token element — this intentional conditional write is
	// the correct pattern here because $derived cannot model "remember the
	// previous non-token value" without side effects.
	// "previousSelection" tracks the last non-token selected element for the
	// token inspector's back-navigation label.  A reactive class encapsulates
	// the conditional-update logic so no $state write happens inside $effect.
	class SelectionTracker {
		value = $state<SelectedElement>(null);
		update(current: SelectedElement) {
			if (current?.type !== 'token') this.value = current ?? null;
		}
	}
	const tracker = new SelectionTracker();
	const previousSelection = $derived.by(() => {
		tracker.update(selectedElement);
		return tracker.value;
	});

	// ---------------------------------------------------------------------------
	// Token analysis helpers
	// ---------------------------------------------------------------------------

	function isLeaseToken(token: { color: { type: string; value?: unknown } }): boolean {
		if (token.color.type !== 'Data' || !token.color.value) return false;
		const data = token.color.value as Record<string, unknown>;
		return 'job_id' in data && 'worker_id' in data;
	}

	function hasCoordinationProvenance(token: { color: { type: string; value?: unknown } }): boolean {
		if (token.color.type !== 'Data' || !token.color.value) return false;
		const data = token.color.value as Record<string, unknown>;
		return '_provenance' in data && typeof data._provenance === 'object';
	}

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

	function formatDuration(start: string, end: string): string {
		const startMs = new Date(start).getTime();
		const endMs = new Date(end).getTime();
		const durationMs = endMs - startMs;
		if (durationMs < 1000) return `${durationMs}ms`;
		return `${(durationMs / 1000).toFixed(2)}s`;
	}

	function getLeaseJobId(token: { color: { type: string; value?: unknown } }): string | null {
		if (!isLeaseToken(token)) return null;
		const data = token.color.value as Record<string, unknown>;
		return data.job_id as string;
	}

	// ---------------------------------------------------------------------------
	// Display helpers
	// ---------------------------------------------------------------------------

	function formatGuard(guard: any): string {
		if (!guard || guard.type === 'Always') return 'Always (no guard)';
		switch (guard.type) {
			case 'IntegerGreaterThan': return `Integer > ${guard.value}`;
			case 'IntegerLessThan': return `Integer < ${guard.value}`;
			case 'DataHasField': return `Has field "${guard.field}"`;
			case 'FieldCompare': return `${guard.field} ${guard.op} ${JSON.stringify(guard.value)}`;
			case 'ColorEquals': return `Color equals ${JSON.stringify(guard.value)}`;
			default: return guard.type;
		}
	}

	function formatJson(value: unknown): string {
		return JSON.stringify(value, null, 2);
	}

	// ---------------------------------------------------------------------------
	// Actions
	// ---------------------------------------------------------------------------

	async function handleInjectToken() {
		if (!placeDetails || !onInjectToken) return;
		injectError = null;
		injectSuccess = false;

		try {
			const data = JSON.parse(injectJsonInput);
			const result = await onInjectToken(placeDetails.place.id, data);
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

	async function handleSimulateCancel() {
		if (!tokenDetails || !isLeaseToken(tokenDetails.token) || !onInjectToken) return;
		const jobId = getLeaseJobId(tokenDetails.token);
		if (!jobId) return;

		cancelInProgress = true;
		cancelError = null;
		cancelSuccess = false;

		try {
			const result = await onInjectToken('p_sig_cancel', { correlation_id: jobId });
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

	function goToEvent(sequence: number) {
		onSelectEvent?.(sequence);
	}
</script>

<div class="inspector h-full bg-card border-l border-border flex flex-col">
	<!-- Header -->
	<div class="px-3 py-2 border-b border-border bg-muted flex items-center justify-between">
		<h3 class="font-semibold text-foreground text-sm">Inspector</h3>
		{#if selectedElement}
			<button
				onclick={() => onClearSelection?.()}
				class="text-muted-foreground hover:text-foreground text-sm"
			>
				Clear
			</button>
		{/if}
	</div>

	<!-- Content -->
	<div class="flex-1 overflow-y-auto p-4">
		{#if !selectedElement}
			<div class="text-muted-foreground text-sm text-center py-8">
				<p>Click on a place or transition to inspect it</p>
			</div>
		{:else if selectedElement.type === 'place' && placeDetails}
			<!-- Place Inspector -->
			<div class="space-y-4">
				<Card tone="muted">
					<h3 class="text-lg font-medium text-foreground">{placeDetails.place.name}</h3>
					<p class="text-xs text-muted-foreground font-mono">{placeDetails.place.id}</p>
					<div class="flex items-center gap-2 mt-2">
						<NodeKindBadge kind={((placeDetails.place as any).kind ?? 'place') as any} />
						{#if placeDetails.place.capacity}
							<span class="text-xs text-muted-foreground">
								Capacity: <span class="font-medium">{placeDetails.place.capacity}</span>
							</span>
						{/if}
					</div>
				</Card>

				<Separator />

				<!-- Tokens List -->
				<Card tone="muted">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
						Tokens ({placeDetails.tokens.length})
					</h4>
					{#if placeDetails.tokens.length === 0}
						<p class="text-sm text-muted-foreground italic">No tokens</p>
					{:else}
						<div class="space-y-2 max-h-48 overflow-y-auto">
							{#each placeDetails.tokens as token (token.id)}
								<button
									class="w-full text-left p-2 rounded border transition-colors {isLeaseToken(token)
										? 'border-l-2 border-l-warning border-warning/30 bg-warning/10 hover:border-warning/50 hover:bg-warning/20'
										: 'border-l-2 border-l-primary/50 border-border hover:border-primary/50 hover:bg-primary/10'}"
									onclick={() => onSelectToken?.(placeDetails.place.id, token.id)}
								>
									<div class="flex items-start gap-2">
										<Badge variant="muted" size="xs" class="shrink-0">{token.color.type}</Badge>
										{#if isLeaseToken(token)}
											<NodeKindBadge kind="lease" size="xs" class="shrink-0" />
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
				</Card>

				<Separator />

				<!-- Token Injection -->
				<div>
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">Inject Token</h4>
					<textarea
						bind:value={injectJsonInput}
						placeholder={'{"amount": 500}'}
						class="w-full h-20 text-sm font-mono p-2 border border-input rounded bg-muted text-foreground resize-none"
						spellcheck="false"
					></textarea>
					{#if injectError}
						<p class="text-xs text-destructive mt-1">{injectError}</p>
					{/if}
					{#if injectSuccess}
						<p class="text-xs text-success mt-1">Token injected!</p>
					{/if}
					<button
						onclick={handleInjectToken}
						disabled={loading}
						class="mt-2 w-full px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50"
					>
						{loading ? 'Injecting...' : 'Inject Token'}
					</button>
				</div>
			</div>
		{:else if selectedElement.type === 'transition' && transitionDetails}
			<!-- Transition Inspector -->
			<div class="space-y-4">
				<Card tone="muted">
					<h3 class="text-lg font-medium text-foreground">{transitionDetails.transition.name}</h3>
					<p class="text-xs text-muted-foreground font-mono">{transitionDetails.transition.id}</p>
					<div class="flex items-center gap-2 mt-2">
						{#if (transitionDetails.transition as any).effect_handler_id}
							<NodeKindBadge kind="effect" />
							<span class="text-xs font-mono text-muted-foreground">
								{(transitionDetails.transition as any).effect_handler_id}
							</span>
						{:else}
							<NodeKindBadge kind="rhai" />
						{/if}
					</div>
				</Card>

				<Separator />

				<!-- Effect Handler -->
				{#if (transitionDetails.transition as any).effect_handler_id}
					<Card tone="muted">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">Effect Handler</h4>
						<div class="px-3 py-2 rounded text-sm bg-secondary border border-border text-secondary-foreground font-mono">
							{(transitionDetails.transition as any).effect_handler_id}
						</div>
						<p class="text-xs text-muted-foreground mt-2">
							Runs a registered side-effect handler instead of a Rhai script.
						</p>
					</Card>

					<Separator />
				{/if}

				<!-- Guard -->
				{#if true}
					{@const guardScript = (transitionDetails.transition as any).guard as string | null}
					<Card tone="muted">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">Guard Condition</h4>
						<div
							class="px-3 py-2 rounded text-sm font-mono {guardScript
								? 'bg-warning/10 border border-warning/30 text-warning-foreground'
								: 'bg-muted text-muted-foreground'}"
						>
							{guardScript ?? 'None (always enabled)'}
						</div>
					</Card>
				{/if}

				<Separator />

				<!-- Input Places -->
				<Card tone="muted">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
						Input Places ({transitionDetails.inputArcs.length})
					</h4>
					{#if transitionDetails.inputArcs.length === 0}
						<p class="text-sm text-muted-foreground italic">None</p>
					{:else}
						<ul class="space-y-1">
							{#each transitionDetails.inputArcs as arc (arc.place_id)}
								<li>
									<button
										class="text-sm text-primary hover:underline"
										onclick={() => onSelectPlace?.(arc.place_id)}
									>
										{getPlaceName?.(arc.place_id) ?? arc.place_name ?? arc.place_id}
									</button>
									{#if arc.weight && arc.weight > 1}
										<span class="text-xs text-muted-foreground">(weight: {arc.weight})</span>
									{/if}
								</li>
							{/each}
						</ul>
					{/if}
				</Card>

				<!-- Output Places -->
				<Card tone="muted">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
						Output Places ({transitionDetails.outputArcs.length})
					</h4>
					{#if transitionDetails.outputArcs.length === 0}
						<p class="text-sm text-muted-foreground italic">None</p>
					{:else}
						<ul class="space-y-1">
							{#each transitionDetails.outputArcs as arc (arc.place_id)}
								<li>
									<button
										class="text-sm text-primary hover:underline"
										onclick={() => onSelectPlace?.(arc.place_id)}
									>
										{getPlaceName?.(arc.place_id) ?? arc.place_name ?? arc.place_id}
									</button>
									{#if arc.weight && arc.weight > 1}
										<span class="text-xs text-muted-foreground">(weight: {arc.weight})</span>
									{/if}
								</li>
							{/each}
						</ul>
					{/if}
				</Card>

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
		{:else if selectedElement.type === 'token' && tokenDetails}
			<!-- Token Inspector -->
			<div class="space-y-4">
				<Card tone="muted">
					<button
						class="text-sm text-primary hover:underline mb-2"
						onclick={() => {
							if (previousSelection?.type === 'event') {
								onSelectEvent?.(previousSelection.sequence);
							} else if (selectedElement?.type === 'token') {
								onSelectPlace?.(selectedElement.placeId);
							}
						}}
					>
						&larr; {previousSelection?.type === 'event' ? `Back to Event #${previousSelection.sequence}` : `Back to ${tokenDetails.placeName}`}
					</button>
					<h3 class="text-lg font-medium text-foreground">Token</h3>
					<p class="text-xs text-muted-foreground font-mono">{tokenDetails.token.id}</p>
				</Card>

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
		{:else if selectedElement.type === 'event' && eventDetails}
			<!-- Event Inspector -->
			<div class="space-y-4">
				<Card tone="muted">
					<div class="flex items-start justify-between gap-2">
						<div class="min-w-0">
							<h3 class="text-lg font-medium text-foreground">Event #{eventDetails.event.sequence}</h3>
							<p class="text-xs text-muted-foreground">
								{new Date(eventDetails.event.timestamp).toLocaleString()}
							</p>
						</div>
						<CopyButton
							text={JSON.stringify(eventDetails.event, null, 2)}
							class="shrink-0"
						/>
					</div>
				</Card>

				<!-- Event Type Badge -->
				<div>
					<NodeKindBadge kind={eventDetails.eventTypeName} />
				</div>

				{#if eventDetails.eventTypeName === 'TransitionFired'}
					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Transition</h4>
						<button
							class="text-sm text-primary hover:underline"
							onclick={() => {
								const e = eventDetails.event.event as any;
								onSelectTransition?.(e.transition_id);
							}}
						>
							{eventDetails.transitionName}
						</button>
					</div>

					{#if eventDetails.consumedTokens && eventDetails.consumedTokens.length > 0}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Consumed ({eventDetails.consumedTokens.length})</h4>
							<div class="space-y-0.5">
								{#each eventDetails.consumedTokens as ct (ct.tokenId)}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-destructive">-</span>
										<button class="text-primary hover:underline" onclick={() => onSelectPlace?.(ct.placeId)}>{ct.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { onSelectToken?.(ct.placeId, ct.tokenId); onViewToken?.(); }}
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
								{#each eventDetails.producedTokens as pt (pt.token.id)}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-success">+</span>
										<button class="text-primary hover:underline" onclick={() => onSelectPlace?.(pt.placeId)}>{pt.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { onSelectToken?.(pt.placeId, pt.token.id); onViewToken?.(); }}
										>{pt.token.id.slice(0, 8)}</button>
									</div>
								{/each}
							</div>
						</div>
					{/if}

					{#if eventDetails.readTokens && eventDetails.readTokens.length > 0}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Read ({eventDetails.readTokens.length})</h4>
							<div class="space-y-0.5">
								{#each eventDetails.readTokens as rt (rt.token.id)}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-info">&cir;</span>
										<button class="text-primary hover:underline" onclick={() => onSelectPlace?.(rt.placeId)}>{rt.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { onSelectToken?.(rt.placeId, rt.token.id); onViewToken?.(); }}
										>{rt.token.id.slice(0, 8)}</button>
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
								onSelectTransition?.(e.transition_id);
							}}
						>
							{eventDetails.transitionName}
						</button>
					</div>

					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Handler</h4>
						<Badge variant="success" class="font-mono">{eventDetails.effectHandlerId}</Badge>
					</div>

					{#if eventDetails.consumedTokens && eventDetails.consumedTokens.length > 0}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Consumed ({eventDetails.consumedTokens.length})</h4>
							<div class="space-y-0.5">
								{#each eventDetails.consumedTokens as ct (ct.tokenId)}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-destructive">-</span>
										<button class="text-primary hover:underline" onclick={() => onSelectPlace?.(ct.placeId)}>{ct.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { onSelectToken?.(ct.placeId, ct.tokenId); onViewToken?.(); }}
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
								{#each eventDetails.producedTokens as pt (pt.token.id)}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-success">+</span>
										<button class="text-primary hover:underline" onclick={() => onSelectPlace?.(pt.placeId)}>{pt.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { onSelectToken?.(pt.placeId, pt.token.id); onViewToken?.(); }}
										>{pt.token.id.slice(0, 8)}</button>
									</div>
								{/each}
							</div>
						</div>
					{/if}

					{#if eventDetails.readTokens && eventDetails.readTokens.length > 0}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Read ({eventDetails.readTokens.length})</h4>
							<div class="space-y-0.5">
								{#each eventDetails.readTokens as rt (rt.token.id)}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-info">&cir;</span>
										<button class="text-primary hover:underline" onclick={() => onSelectPlace?.(rt.placeId)}>{rt.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { onSelectToken?.(rt.placeId, rt.token.id); onViewToken?.(); }}
										>{rt.token.id.slice(0, 8)}</button>
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
								onSelectTransition?.(e.transition_id);
							}}
						>
							{eventDetails.transitionName}
						</button>
					</div>

					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Handler</h4>
						<Badge variant="destructive" class="font-mono">{eventDetails.effectHandlerId}</Badge>
					</div>

					<div class="bg-destructive/10 border border-destructive/30 rounded p-2 text-xs text-destructive">
						{eventDetails.errorMessage}
					</div>

					<div class="flex items-center gap-2">
						{#if eventDetails.retryable !== undefined}
							<Badge variant={eventDetails.retryable ? 'warning' : 'destructive'} size="xs">
								{eventDetails.retryable ? 'Retryable' : 'Non-retryable'}
							</Badge>
						{/if}
					</div>

					{#if eventDetails.inputData}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Input Data</h4>
							<pre class="text-[10px] font-mono bg-muted rounded p-2 overflow-x-auto max-h-32 text-foreground/70">{JSON.stringify(eventDetails.inputData, null, 2)}</pre>
						</div>
					{/if}

					{#if eventDetails.consumedTokens && eventDetails.consumedTokens.length > 0}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Consumed ({eventDetails.consumedTokens.length})</h4>
							<div class="space-y-0.5">
								{#each eventDetails.consumedTokens as ct (ct.tokenId)}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-destructive">-</span>
										<button class="text-primary hover:underline" onclick={() => onSelectPlace?.(ct.placeId)}>{ct.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { onSelectToken?.(ct.placeId, ct.tokenId); onViewToken?.(); }}
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
								{#each eventDetails.producedTokens as pt (pt.token.id)}
									<div class="flex items-center gap-2 text-xs">
										<span class="text-success">+</span>
										<button class="text-primary hover:underline" onclick={() => onSelectPlace?.(pt.placeId)}>{pt.placeName}</button>
										<button
											class="text-muted-foreground font-mono hover:text-primary hover:underline"
											onclick={() => { onSelectToken?.(pt.placeId, pt.token.id); onViewToken?.(); }}
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
								onSelectPlace?.(e.place_id);
							}}
						>
							{eventDetails.placeName}
						</button>
					</div>

					<div class="flex items-center gap-2">
						<span class="text-xs text-muted-foreground font-mono">{eventDetails.token.id.slice(0, 8)}</span>
						<button
							onclick={() => { onSelectToken?.((eventDetails.event.event as any).place_id, eventDetails.token!.id); onViewToken?.(); }}
							class="px-3 py-1.5 text-xs font-medium rounded bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
						>
							View Details
						</button>
					</div>

					{#if eventDetails.signalKey}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Signal Key</h4>
							<span class="text-xs font-mono text-foreground/80 break-all">{eventDetails.signalKey}</span>
						</div>
					{/if}
					{#if eventDetails.workflowId}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Workflow ID</h4>
							<span class="text-xs font-mono text-foreground/80 break-all">{eventDetails.workflowId}</span>
						</div>
					{/if}

				{:else if eventDetails.eventTypeName === 'TokenConsumed'}
					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">From Place</h4>
						<button
							class="text-sm text-primary hover:underline"
							onclick={() => {
								const e = eventDetails.event.event as any;
								onSelectPlace?.(e.place_id);
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
								onSelectTransition?.(e.transition_id);
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
								onSelectPlace?.(e.source_place_id);
							}}
						>
							{eventDetails.placeName}
						</button>
					</div>

					<div>
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Target</h4>
						<NodeKindBadge kind="bridge_out" label="{eventDetails.targetNetId} / {eventDetails.targetPlaceName}" />
					</div>

					{#if eventDetails.token}
						<div class="flex items-center gap-2">
							<span class="text-xs text-muted-foreground font-mono">{eventDetails.token.id.slice(0, 8)}</span>
							<button
								onclick={() => { onSelectToken?.((eventDetails.event.event as any).source_place_id, eventDetails.token!.id); onViewToken?.(); }}
								class="px-3 py-1.5 text-xs font-medium rounded bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
							>
								View Details
							</button>
						</div>
					{/if}

					{#if eventDetails.signalKey}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Signal Key</h4>
							<span class="text-xs font-mono text-foreground/80 break-all">{eventDetails.signalKey}</span>
						</div>
					{/if}
					{#if eventDetails.replyToPlaceName}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Reply To</h4>
							<span class="text-xs font-medium text-foreground/80">{eventDetails.replyToPlaceName}</span>
						</div>
					{/if}
					{#if eventDetails.replyChannels}
						<div>
							<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Reply Channels</h4>
							<div class="space-y-0.5">
								{#each Object.entries(eventDetails.replyChannels) as [channel, place] (channel)}
									<div class="text-xs">
										<span class="font-mono text-destructive">{channel}</span>
										<span class="text-muted-foreground mx-1">&rarr;</span>
										<span class="font-medium text-foreground/80">{place}</span>
									</div>
								{/each}
							</div>
						</div>
					{/if}

				{:else if eventDetails.eventTypeName === 'ErrorOccurred'}
					<div class="bg-destructive/10 border border-destructive/30 rounded p-2 text-xs text-destructive">
						{eventDetails.errorMessage}
					</div>

				{:else if eventDetails.eventTypeName === 'NetInitialized'}
					<div class="text-sm text-muted-foreground">
						Net initialized with its topology and tokens.
					</div>
				{/if}

				<Separator />

				<!-- Hash Chain -->
				<Card tone="muted">
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
				</Card>
			</div>
		{:else if selectedElement.type === 'group' && groupDetails}
			<!-- Group Inspector (collapsed meta-node) -->
			<div class="space-y-4">
				<Card tone="muted">
					<h3 class="text-lg font-medium text-foreground">{groupDetails.group.name}</h3>
					<p class="text-xs text-muted-foreground font-mono">{groupDetails.group.id}</p>
					<div class="flex items-center gap-2 mt-2">
						<NodeKindBadge kind="group" />
						<span class="text-xs text-muted-foreground">
							{groupDetails.places.length} places · {groupDetails.transitions.length} transitions
						</span>
					</div>
				</Card>

				{#if groupDetails.childGroups.length > 0}
					<Separator />
					<Card tone="muted">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
							Sub-groups ({groupDetails.childGroups.length})
						</h4>
						<div class="space-y-1">
							{#each groupDetails.childGroups as child (child.id)}
								<button
									class="w-full text-left px-2 py-1 rounded border border-border hover:border-primary/50 hover:bg-primary/10 transition-colors"
									onclick={() => onSelectGroup?.(child.id)}
								>
									<span class="text-xs font-medium text-foreground">{child.name}</span>
								</button>
							{/each}
						</div>
					</Card>
				{/if}

				<Separator />

				<!-- Places in group -->
				<Card tone="muted">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
						Places ({groupDetails.places.length})
					</h4>
					<div class="space-y-1 max-h-40 overflow-y-auto">
						{#each groupDetails.places as place (place.id)}
							{@const count = groupDetails.allTokens.filter((t: any) => t.placeId === place.id).length}
							<button
								class="w-full text-left px-2 py-1 rounded border border-border hover:border-primary/50 hover:bg-primary/10 transition-colors flex items-center gap-2"
								onclick={() => onSelectPlace?.(place.id)}
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
				</Card>

				<Separator />

				<!-- Tokens across all places -->
				<Card tone="muted">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
						Tokens ({groupDetails.allTokens.length})
					</h4>
					{#if groupDetails.allTokens.length === 0}
						<p class="text-sm text-muted-foreground italic">No tokens in this group</p>
					{:else}
						<div class="space-y-2 max-h-64 overflow-y-auto">
							{#each groupDetails.allTokens as { placeId, placeName, token } (token.id)}
								<button
									class="w-full text-left p-2 rounded border border-l-2 border-l-primary/50 border-border hover:border-primary/50 hover:bg-primary/10 transition-colors"
									onclick={() => onSelectToken?.(placeId, token.id)}
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
				</Card>

				<Separator />

				<!-- Transitions in group -->
				<Card tone="muted">
					<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
						Transitions ({groupDetails.transitions.length})
					</h4>
					<div class="space-y-1 max-h-40 overflow-y-auto">
						{#each groupDetails.transitions as transition (transition.id)}
							<button
								class="w-full text-left px-2 py-1 rounded border border-border hover:border-primary/50 hover:bg-primary/10 transition-colors flex items-center gap-2"
								onclick={() => onSelectTransition?.(transition.id)}
							>
								<span class="text-xs font-medium text-foreground truncate">{transition.name}</span>
								{#if (transition as any).effect_handler_id}
									<Badge variant="secondary" size="xs" class="ml-auto font-mono shrink-0">FX</Badge>
								{/if}
							</button>
						{/each}
					</div>
				</Card>
			</div>
		{:else if selectedElement.type === 'remotenet'}
			{@const rn = selectedElement}
			<!-- Remote Net Inspector -->
			<div class="space-y-4">
				<Card tone="muted">
					<h3 class="text-lg font-medium text-foreground">{rn.label}</h3>
					<p class="text-xs text-muted-foreground font-mono">{rn.id}</p>
					<div class="flex items-center gap-2 mt-2">
						<NodeKindBadge kind="remote_net" />
						{#if rn.childNetIds.length > 0}
							<span class="text-xs text-muted-foreground">
								{rn.childNetIds.length} {rn.childNetIds.length === 1 ? 'instance' : 'instances'}
							</span>
						{/if}
					</div>
				</Card>

				<Separator />

				<!-- Bridge Ports -->
				{#if rn.targets.length > 0}
					<Card tone="muted">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
							Outbound Ports ({rn.targets.length})
						</h4>
						<div class="space-y-1">
							{#each rn.targets as port (port)}
								<div class="px-2 py-1 rounded border border-border flex items-center gap-2">
									<span class="w-2 h-2 rounded-full bg-destructive shrink-0"></span>
									<span class="text-xs font-mono text-foreground truncate">{port}</span>
								</div>
							{/each}
						</div>
					</Card>
				{/if}

				{#if rn.sources.length > 0}
					<Card tone="muted">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
							Inbound Ports ({rn.sources.length})
						</h4>
						<div class="space-y-1">
							{#each rn.sources as port (port)}
								<div class="px-2 py-1 rounded border border-border flex items-center gap-2">
									<span class="w-2 h-2 rounded-full bg-success shrink-0"></span>
									<span class="text-xs font-mono text-foreground truncate">{port}</span>
								</div>
							{/each}
						</div>
					</Card>
				{/if}

				{#if rn.childNetIds.length > 0}
					<Separator />

					<!-- Child Net Instances -->
					<Card tone="muted">
						<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
							Child Instances ({rn.childNetIds.length})
						</h4>
						<div class="space-y-1 max-h-48 overflow-y-auto">
							{#each rn.childNetIds as childId (childId)}
								<button
									class="w-full text-left px-2 py-1.5 rounded border border-border hover:border-success/50 hover:bg-success/10 transition-colors flex items-center gap-2"
									onclick={() => onNavigateToChild?.(childId)}
								>
									<span class="text-xs font-mono text-foreground truncate">{childId.slice(0, 12)}...</span>
									<ExternalLink class="w-3 h-3 ml-auto shrink-0 text-success" />
								</button>
							{/each}
						</div>
					</Card>
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
