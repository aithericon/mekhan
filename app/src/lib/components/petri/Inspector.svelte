<script lang="ts">
	import type { SelectedElement } from '$lib/types/petri';
	import { Button } from '$lib/components/ui/button';
	import { isLeaseToken, getLeaseJobId } from '$lib/petri/token-analysis';
	import type {
		PlaceDetails,
		TransitionDetails,
		TokenDetails,
		EventDetails
	} from '$lib/stores/inspector-selectors';
	import PlaceInspector from './inspector/PlaceInspector.svelte';
	import TransitionInspector from './inspector/TransitionInspector.svelte';
	import TokenInspector from './inspector/TokenInspector.svelte';
	import EventInspector from './inspector/EventInspector.svelte';
	import GroupInspector from './inspector/GroupInspector.svelte';
	import RemoteNetInspector from './inspector/RemoteNetInspector.svelte';

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
		eventDetails?: EventDetails | null;
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
			<Button variant="ghost" size="xs" onclick={() => onClearSelection?.()}>
				Clear
			</Button>
		{/if}
	</div>

	<!-- Content — this component is now a thin router; each branch is its
	     own inspector component under ./inspector/. -->
	<div class="flex-1 overflow-y-auto p-4">
		{#if !selectedElement}
			<div class="text-muted-foreground text-sm text-center py-8">
				<p>Click on a place or transition to inspect it</p>
			</div>
		{:else if selectedElement.type === 'place' && placeDetails}
			<PlaceInspector
				{placeDetails}
				{loading}
				{injectJsonInput}
				{injectError}
				{injectSuccess}
				{onSelectToken}
				onInjectToken={handleInjectToken}
				onInjectInput={(v) => (injectJsonInput = v)}
			/>
		{:else if selectedElement.type === 'transition' && transitionDetails}
			<TransitionInspector
				{transitionDetails}
				{getPlaceName}
				{onSelectPlace}
				{onOpenScript}
			/>
		{:else if selectedElement.type === 'token' && tokenDetails}
			<TokenInspector
				{tokenDetails}
				{selectedElement}
				{previousSelection}
				{onSelectPlace}
				{onSelectEvent}
				{onViewToken}
			/>
		{:else if selectedElement.type === 'event' && eventDetails}
			<EventInspector
				{eventDetails}
				{onSelectPlace}
				{onSelectTransition}
				{onSelectToken}
				{onViewToken}
			/>
		{:else if selectedElement.type === 'group' && groupDetails}
			<GroupInspector
				{groupDetails}
				{onSelectPlace}
				{onSelectTransition}
				{onSelectToken}
				{onSelectGroup}
			/>
		{:else if selectedElement.type === 'remotenet'}
			{@const rn = selectedElement}
			<RemoteNetInspector {rn} {onNavigateToChild} />
		{/if}
	</div>
</div>
