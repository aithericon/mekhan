/**
 * Petri-lab domain types.
 *
 * These model the core concepts from petri-lab's Colored Petri Net engine.
 * Designed for future extraction into @aithericon/petri-core.
 */

/** Token color discriminated union — matches petri-lab's TokenColor enum */
export type TokenColor =
	| { type: 'Unit' }
	| { type: 'Integer'; value: number }
	| { type: 'Data'; value: unknown };

/** A single token in a Petri net place */
export type Token = {
	id: string;
	color: TokenColor;
	created_at: string;
	created_by_event?: number;
	bridge_meta?: {
		correlation_id: string;
		source_net_id: string;
		reply_to?: { net_id: string; place_name: string };
	};
};

/**
 * The marking of a Petri net — the distribution of tokens across places.
 *
 * petri-lab returns this as: { tokens: Record<string, Token[]> }
 * where keys are place IDs and values are arrays of tokens at that place.
 */
export type Marking = {
	tokens: Record<string, Token[]>;
};

/** Get a human-readable label for a token color */
export function tokenColorLabel(color: TokenColor): string {
	switch (color.type) {
		case 'Unit':
			return 'Unit';
		case 'Integer':
			return String(color.value);
		case 'Data':
			return 'Data';
	}
}

// ---------------------------------------------------------------------------
// Engine status
// ---------------------------------------------------------------------------

export type EngineStatus = {
	available: boolean;
	run_mode: string | null;
};

// ---------------------------------------------------------------------------
// Event log types (matches petri-lab's PersistedEvent / DomainEvent)
// ---------------------------------------------------------------------------

/** Persisted event from JetStream event log */
export type PersistedEvent = {
	sequence: number;
	timestamp: string;
	event: DomainEvent;
	hash: string;
	previous_hash: string | null;
};

/** Domain event discriminated union */
export type DomainEvent =
	| { type: 'NetInitialized'; net: unknown }
	| { type: 'TokenCreated'; token: Token; place_id: string; place_name?: string }
	| {
			type: 'TransitionFired';
			transition_id: string;
			transition_name?: string;
			consumed_tokens: [string, string][];
			produced_tokens: [string, Token][];
		}
	| {
			type: 'EffectCompleted';
			transition_id: string;
			transition_name?: string;
			consumed_tokens: [string, string][];
			produced_tokens: [string, Token][];
			effect_handler_id: string;
			effect_result: unknown;
		}
	| {
			type: 'EffectFailed';
			transition_id: string;
			transition_name?: string;
			error_message: string;
			tokens_consumed: boolean;
		}
	| { type: 'TokenConsumed'; token_id: string; place_id: string }
	| { type: 'TokenRemoved'; token_id: string; place_id: string; reason?: string }
	| { type: 'TokenUpdated'; token_id: string; place_id: string; new_color: TokenColor }
	| {
			type: 'TokenBridgedOut';
			token: Token;
			source_place_id: string;
			target_net_id: string;
			target_place_name: string;
		}
	| { type: 'NetCreated'; net_id: string }
	| { type: 'NetCompleted'; net_id: string; terminal_place_id: string }
	| { type: 'NetCancelled'; net_id: string; reason?: string }
	| { type: 'ErrorOccurred'; message: string };

// ---------------------------------------------------------------------------
// Topology types (matches petri-lab's PetriNet / Place / Transition / Arc)
// ---------------------------------------------------------------------------

export type ArcDirection = 'place_to_transition' | 'transition_to_place';

export type Arc = {
	transition_id: string;
	place_id: string;
	direction: ArcDirection;
	weight?: number;
	read?: boolean;
};

export type PlaceKind = 'internal' | 'signal' | 'bridge_in' | 'bridge_out' | 'bridge_reply' | 'terminal';

export type Place = {
	id: string;
	name: string;
	kind?: PlaceKind;
	capacity?: number | null;
	token_schema?: string | null;
	bridge_target?: {
		target_net_id: string;
		target_place_name: string;
		reply_to?: string;
	} | null;
	bridge_source?: {
		source_net_id: string;
		source_place_name: string;
	} | null;
};

export type Port = {
	name: string;
	schema_ref?: string | null;
	cardinality?: 'single' | 'batch';
};

export type Transition = {
	id: string;
	name: string;
	guard?: string | null;
	script?: string;
	input_ports?: Port[];
	output_ports?: Port[];
	effect_handler_id?: string | null;
	logic_type?: 'rhai' | 'wasm' | 'effect';
};

export type PetriNet = {
	places: Place[];
	transitions: Transition[];
	arcs: Arc[];
};

export type TransitionStatus =
	| 'enabled'
	| 'disabled_no_tokens'
	| 'disabled_guard_failed'
	| 'disabled_guard_error';

// ---------------------------------------------------------------------------
// Scenario / groups
// ---------------------------------------------------------------------------

export type ScenarioGroup = {
	id: string;
	name: string;
	parent_id?: string | null;
	metadata?: Record<string, unknown>;
};

// ---------------------------------------------------------------------------
// Visualization types (used by Petri visualizer components)
// ---------------------------------------------------------------------------

export type SelectedElement =
	| { type: 'place'; id: string }
	| { type: 'transition'; id: string }
	| { type: 'token'; placeId: string; tokenId: string }
	| { type: 'event'; sequence: number }
	| { type: 'group'; id: string }
	| {
			type: 'remotenet';
			id: string;
			label: string;
			targets: string[];
			sources: string[];
			childNetIds: string[];
		}
	| null;

export type EventSpotlight = {
	transitionId: string | null;
	consumedPlaceIds: string[];
	producedPlaceIds: string[];
	targetPlaceId: string | null;
	allNodeIds: string[];
};

export type MarkingDiff = {
	appeared: string[];
	disappeared: string[];
	firedTransition: string | null;
};

export type IssueLevel = 'error' | 'warning' | 'info';

export type ValidationIssue = {
	level: IssueLevel;
	message: string;
	node_id?: string;
	node_type?: 'place' | 'transition';
};

export type AnalysisReport = {
	issues: ValidationIssue[];
};

// ---------------------------------------------------------------------------
// Multi-net metadata
// ---------------------------------------------------------------------------

export type NetMeta = {
	net_id: string;
	label?: string;
	status: string;
	in_memory: boolean;
	template_id?: string;
	created_by?: string;
};
