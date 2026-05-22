/**
 * Inspector view-model selectors for the petri store.
 *
 * Pure functions that turn the current selection + projected state into the
 * shapes the Inspector component renders. Extracted from `petri.svelte.ts`
 * so the store stays a thin orchestrator. The `getSelectedEventDetails`
 * switch now narrows the typed `DomainEvent` discriminated union instead of
 * casting `event.event as any` per branch, with a shared token-ref mapper.
 */

import type {
	PersistedEvent,
	PetriNet,
	ScenarioGroup,
	SelectedElement,
	Token
} from '$lib/types/petri';

/** Resolves a node id to a human-readable name (falls back to the id). */
export type NameResolver = (id: string) => string;

export interface PlaceDetails {
	place: PetriNet['places'][number];
	tokens: Token[];
}

export interface TransitionArc {
	place_id: string;
	place_name: string;
	weight?: number;
}

export interface TransitionDetails {
	transition: PetriNet['transitions'][number];
	inputArcs: TransitionArc[];
	outputArcs: TransitionArc[];
}

export interface TokenDetails {
	token: Token;
	placeName: string;
	creationEvent?: PersistedEvent;
}

export interface TokenRef {
	placeId: string;
	placeName: string;
	tokenId: string;
}

export interface ProducedTokenRef {
	placeId: string;
	placeName: string;
	token: Token;
}

export interface EventDetails {
	event: PersistedEvent;
	eventTypeName: string;
	transitionName?: string;
	placeName?: string;
	consumedTokens?: TokenRef[];
	producedTokens?: ProducedTokenRef[];
	readTokens?: ProducedTokenRef[];
	token?: Token;
	errorMessage?: string;
	targetNetId?: string;
	targetPlaceName?: string;
	correlationId?: string;
	replyToPlaceName?: string;
	replyChannels?: Record<string, string>;
	signalKey?: string;
	workflowId?: string;
	effectHandlerId?: string;
	effectResult?: unknown;
	inputData?: Record<string, unknown>;
	retryable?: boolean;
}

export interface GroupDetails {
	group: ScenarioGroup;
}

export function getSelectedPlaceDetails(
	sel: SelectedElement,
	topology: PetriNet | null,
	projectedMarking: Map<string, Token[]>
): PlaceDetails | null {
	if (!sel || sel.type !== 'place' || !topology) return null;
	const place = topology.places.find((p) => p.id === sel.id);
	if (!place) return null;
	const tokens = projectedMarking.get(place.id) ?? [];
	return { place, tokens };
}

export function getSelectedTransitionDetails(
	sel: SelectedElement,
	topology: PetriNet | null,
	getPlaceName: NameResolver
): TransitionDetails | null {
	if (!sel || sel.type !== 'transition' || !topology) return null;
	const transition = topology.transitions.find((t) => t.id === sel.id);
	if (!transition) return null;
	const inputArcs = topology.arcs
		.filter((a) => a.transition_id === transition.id && a.direction === 'place_to_transition')
		.map((a) => ({ place_id: a.place_id, place_name: getPlaceName(a.place_id), weight: a.weight }));
	const outputArcs = topology.arcs
		.filter((a) => a.transition_id === transition.id && a.direction === 'transition_to_place')
		.map((a) => ({ place_id: a.place_id, place_name: getPlaceName(a.place_id), weight: a.weight }));
	return { transition, inputArcs, outputArcs };
}

export function getSelectedTokenDetails(
	sel: SelectedElement,
	projectedMarking: Map<string, Token[]>,
	bridgedOutTokens: Map<string, Token[]>,
	events: PersistedEvent[],
	getPlaceName: NameResolver
): TokenDetails | null {
	if (!sel || sel.type !== 'token') return null;
	const tokens = projectedMarking.get(sel.placeId) ?? [];
	let token = tokens.find((t) => t.id === sel.tokenId);
	// Also search bridged-out tokens (ghost tokens in outboxes)
	if (!token) {
		const bridged = bridgedOutTokens.get(sel.placeId) ?? [];
		token = bridged.find((t) => t.id === sel.tokenId);
	}
	if (!token) return null;
	const found = token;
	const placeName = getPlaceName(sel.placeId);
	const creationEvent = events.find(
		(e) => e.event.type === 'TokenCreated' && e.event.token?.id === found.id
	);
	return { token: found, placeName, creationEvent };
}

/** Map `[placeId, tokenId]` pairs to inspector token refs (resolving names). */
function mapTokenRefs(
	pairs: [string, string][] | undefined,
	getPlaceName: NameResolver
): TokenRef[] | undefined {
	return pairs?.map(([placeId, tokenId]) => ({
		placeId,
		placeName: getPlaceName(placeId),
		tokenId
	}));
}

/** Map `[placeId, Token]` pairs to inspector produced/read token refs. */
function mapProducedRefs(
	pairs: [string, Token][] | undefined,
	getPlaceName: NameResolver
): ProducedTokenRef[] | undefined {
	return pairs?.map(([placeId, token]) => ({
		placeId,
		placeName: getPlaceName(placeId),
		token
	}));
}

export function getSelectedEventDetails(
	sel: SelectedElement,
	events: PersistedEvent[],
	getTransitionName: NameResolver,
	getPlaceName: NameResolver,
	resolveErrorMessage: (msg: string) => string
): EventDetails | null {
	if (!sel || sel.type !== 'event') return null;
	const event = events.find((e) => e.sequence === sel.sequence);
	if (!event) return null;

	const details: EventDetails = {
		event,
		eventTypeName: event.event.type
	};

	const ev = event.event;
	switch (ev.type) {
		case 'TransitionFired': {
			details.transitionName = getTransitionName(ev.transition_id);
			details.consumedTokens = mapTokenRefs(ev.consumed_tokens, getPlaceName);
			details.producedTokens = mapProducedRefs(ev.produced_tokens, getPlaceName);
			if (ev.read_tokens?.length) {
				details.readTokens = mapProducedRefs(ev.read_tokens, getPlaceName);
			}
			break;
		}
		case 'TokenCreated': {
			details.placeName = getPlaceName(ev.place_id);
			details.token = ev.token;
			if (ev.signal_key) details.signalKey = ev.signal_key;
			if (ev.workflow_id) details.workflowId = ev.workflow_id;
			break;
		}
		case 'TokenConsumed': {
			details.placeName = getPlaceName(ev.place_id);
			break;
		}
		case 'NetInitialized': {
			break;
		}
		case 'TokenBridgedOut': {
			details.transitionName = ev.transition_id
				? getTransitionName(ev.transition_id)
				: undefined;
			details.placeName = getPlaceName(ev.source_place_id);
			details.token = ev.token;
			details.targetNetId = ev.target_net_id;
			details.targetPlaceName = ev.target_place_name;
			if (ev.signal_key) details.signalKey = ev.signal_key;
			details.replyToPlaceName = ev.reply_to_place_name ?? undefined;
			if (ev.reply_channels) details.replyChannels = ev.reply_channels;
			break;
		}
		case 'EffectCompleted': {
			details.transitionName = getTransitionName(ev.transition_id);
			details.effectHandlerId = ev.effect_handler_id;
			details.effectResult = ev.effect_result;
			details.consumedTokens = mapTokenRefs(ev.consumed_tokens, getPlaceName);
			details.producedTokens = mapProducedRefs(ev.produced_tokens, getPlaceName);
			if (ev.read_tokens?.length) {
				details.readTokens = mapProducedRefs(ev.read_tokens, getPlaceName);
			}
			break;
		}
		case 'EffectFailed': {
			details.transitionName = getTransitionName(ev.transition_id);
			details.effectHandlerId = ev.effect_handler_id;
			details.errorMessage = ev.error_message ?? 'Effect failed';
			details.retryable = ev.retryable ?? true;
			if (ev.input_data) details.inputData = ev.input_data;
			details.consumedTokens = mapTokenRefs(ev.consumed_tokens, getPlaceName);
			details.producedTokens = mapProducedRefs(ev.produced_tokens, getPlaceName);
			break;
		}
		case 'ErrorOccurred': {
			details.errorMessage = resolveErrorMessage(ev.message ?? '');
			break;
		}
	}

	return details;
}

export function getSelectedGroupDetails(
	sel: SelectedElement,
	currentGroups: ScenarioGroup[]
): GroupDetails | null {
	if (!sel || sel.type !== 'group') return null;
	const group = currentGroups.find((g) => g.id === sel.id);
	if (!group) return null;
	return { group };
}
