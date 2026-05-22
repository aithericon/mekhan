<script lang="ts">
	import { Separator } from '$lib/components/ui/separator';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import { Card } from '$lib/components/ui/card';
	import { Badge } from '$lib/components/ui/badge';
	import NodeKindBadge from '../NodeKindBadge.svelte';
	import { Button } from '$lib/components/ui/button';
	import TokenRefList from './TokenRefList.svelte';
	import type { DomainEvent } from '$lib/types/petri';
	import type { EventDetails } from '$lib/stores/inspector-selectors';

	interface Props {
		eventDetails: EventDetails;
		onSelectPlace?: (id: string) => void;
		onSelectTransition?: (id: string) => void;
		onSelectToken?: (placeId: string, tokenId: string) => void;
		onViewToken?: () => void;
	}

	let {
		eventDetails,
		onSelectPlace,
		onSelectTransition,
		onSelectToken,
		onViewToken
	}: Props = $props();

	// The raw domain event behind the projected details — used only to recover
	// ids for the click handlers (the original read `event.event as any`).
	const ev = $derived(eventDetails.event.event as DomainEvent);

	function transitionId(): string | undefined {
		return 'transition_id' in ev ? ev.transition_id : undefined;
	}
	function placeId(): string | undefined {
		return 'place_id' in ev ? ev.place_id : undefined;
	}
	function sourcePlaceId(): string | undefined {
		return 'source_place_id' in ev ? ev.source_place_id : undefined;
	}
</script>

<div class="space-y-4">
	<Card tone="muted">
		<div class="flex items-start justify-between gap-2">
			<div class="min-w-0">
				<h3 class="text-lg font-medium text-foreground">Event #{eventDetails.event.sequence}</h3>
				<p class="text-sm text-muted-foreground">
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
		<NodeKindBadge kind={eventDetails.eventTypeName as any} />
	</div>

	{#if eventDetails.eventTypeName === 'TransitionFired'}
		<div>
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Transition</h4>
			<Button variant="link" size="inline" class="text-sm" onclick={() => onSelectTransition?.(transitionId()!)}>
				{eventDetails.transitionName}
			</Button>
		</div>

		<TokenRefList kind="consumed" refs={eventDetails.consumedTokens ?? []} {onSelectPlace} {onSelectToken} {onViewToken} />
		<TokenRefList kind="produced" refs={eventDetails.producedTokens ?? []} {onSelectPlace} {onSelectToken} {onViewToken} />
		<TokenRefList kind="read" refs={eventDetails.readTokens ?? []} {onSelectPlace} {onSelectToken} {onViewToken} />

	{:else if eventDetails.eventTypeName === 'EffectCompleted'}
		<div>
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Transition</h4>
			<Button variant="link" size="inline" class="text-sm" onclick={() => onSelectTransition?.(transitionId()!)}>
				{eventDetails.transitionName}
			</Button>
		</div>

		<div>
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Handler</h4>
			<Badge variant="success" class="font-mono">{eventDetails.effectHandlerId}</Badge>
		</div>

		<TokenRefList kind="consumed" refs={eventDetails.consumedTokens ?? []} {onSelectPlace} {onSelectToken} {onViewToken} />
		<TokenRefList kind="produced" refs={eventDetails.producedTokens ?? []} {onSelectPlace} {onSelectToken} {onViewToken} />
		<TokenRefList kind="read" refs={eventDetails.readTokens ?? []} {onSelectPlace} {onSelectToken} {onViewToken} />

	{:else if eventDetails.eventTypeName === 'EffectFailed'}
		<div>
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Transition</h4>
			<Button variant="link" size="inline" class="text-sm" onclick={() => onSelectTransition?.(transitionId()!)}>
				{eventDetails.transitionName}
			</Button>
		</div>

		<div>
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Handler</h4>
			<Badge variant="destructive" class="font-mono">{eventDetails.effectHandlerId}</Badge>
		</div>

		<div class="bg-destructive/10 border border-destructive/30 rounded p-2 text-sm text-destructive">
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
				<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Input Data</h4>
				<pre class="text-sm font-mono bg-muted rounded p-2 overflow-x-auto max-h-32 text-foreground/70">{JSON.stringify(eventDetails.inputData, null, 2)}</pre>
			</div>
		{/if}

		<TokenRefList kind="consumed" refs={eventDetails.consumedTokens ?? []} {onSelectPlace} {onSelectToken} {onViewToken} />
		<TokenRefList kind="produced" refs={eventDetails.producedTokens ?? []} {onSelectPlace} {onSelectToken} {onViewToken} />

	{:else if eventDetails.eventTypeName === 'TokenCreated' && eventDetails.token}
		<div>
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Place</h4>
			<Button variant="link" size="inline" class="text-sm" onclick={() => onSelectPlace?.(placeId()!)}>
				{eventDetails.placeName}
			</Button>
		</div>

		<div class="flex items-center gap-2">
			<span class="text-sm text-muted-foreground font-mono">{eventDetails.token.id.slice(0, 8)}</span>
			<Button
				size="xs"
				onclick={() => { onSelectToken?.(placeId()!, eventDetails.token!.id); onViewToken?.(); }}
			>
				View Details
			</Button>
		</div>

		{#if eventDetails.signalKey}
			<div>
				<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Signal Key</h4>
				<span class="text-sm font-mono text-foreground/80 break-all">{eventDetails.signalKey}</span>
			</div>
		{/if}
		{#if eventDetails.workflowId}
			<div>
				<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Workflow ID</h4>
				<span class="text-sm font-mono text-foreground/80 break-all">{eventDetails.workflowId}</span>
			</div>
		{/if}

	{:else if eventDetails.eventTypeName === 'TokenConsumed'}
		<div>
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">From Place</h4>
			<Button variant="link" size="inline" class="text-sm" onclick={() => onSelectPlace?.(placeId()!)}>
				{eventDetails.placeName}
			</Button>
		</div>

	{:else if eventDetails.eventTypeName === 'TokenBridgedOut'}
		<div>
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Transition</h4>
			<Button variant="link" size="inline" class="text-sm" onclick={() => onSelectTransition?.(transitionId()!)}>
				{eventDetails.transitionName}
			</Button>
		</div>

		<div>
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Source</h4>
			<Button variant="link" size="inline" class="text-sm" onclick={() => onSelectPlace?.(sourcePlaceId()!)}>
				{eventDetails.placeName}
			</Button>
		</div>

		<div>
			<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Target</h4>
			<NodeKindBadge kind="bridge_out" label="{eventDetails.targetNetId} / {eventDetails.targetPlaceName}" />
		</div>

		{#if eventDetails.token}
			<div class="flex items-center gap-2">
				<span class="text-sm text-muted-foreground font-mono">{eventDetails.token.id.slice(0, 8)}</span>
				<Button
					size="xs"
					onclick={() => { onSelectToken?.(sourcePlaceId()!, eventDetails.token!.id); onViewToken?.(); }}
				>
					View Details
				</Button>
			</div>
		{/if}

		{#if eventDetails.signalKey}
			<div>
				<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Signal Key</h4>
				<span class="text-sm font-mono text-foreground/80 break-all">{eventDetails.signalKey}</span>
			</div>
		{/if}
		{#if eventDetails.replyToPlaceName}
			<div>
				<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Reply To</h4>
				<span class="text-sm font-medium text-foreground/80">{eventDetails.replyToPlaceName}</span>
			</div>
		{/if}
		{#if eventDetails.replyChannels}
			<div>
				<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">Reply Channels</h4>
				<div class="space-y-0.5">
					{#each Object.entries(eventDetails.replyChannels) as [channel, place] (channel)}
						<div class="text-sm">
							<span class="font-mono text-destructive">{channel}</span>
							<span class="text-muted-foreground mx-1">&rarr;</span>
							<span class="font-medium text-foreground/80">{place}</span>
						</div>
					{/each}
				</div>
			</div>
		{/if}

	{:else if eventDetails.eventTypeName === 'ErrorOccurred'}
		<div class="bg-destructive/10 border border-destructive/30 rounded p-2 text-sm text-destructive">
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
		<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-2">Hash Chain</h4>
		<div class="text-sm font-mono space-y-1">
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
