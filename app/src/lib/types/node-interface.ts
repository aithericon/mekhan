/**
 * Frontend mirror of the Rust `NodeInterface` (`service/src/compiler/interface.rs`).
 *
 * Persisted per template version in `workflow_templates.interface_json` (typed
 * as `unknown` in the OpenAPI schema because the registry is serialised whole),
 * so consumers have to project it. This shape only declares the fields the
 * frontend actually reads — the canonical source of truth is the Rust struct.
 */
export interface NodeInterface {
	node_id: string;
	kind: string;
	entry?: string;
	named_inputs?: Record<string, string>;
	outputs?: Record<string, string>;
	data_port?: string | null;
	workflow_terminals?: string[];
	owned_places?: string[];
	owned_transitions?: string[];
	/**
	 * `producer_node_id → [field_attr, …]` — what this node's author actually
	 * referenced off each upstream parked envelope (Python source for
	 * AutomatedSteps, `{{ <slug>.<attr> }}` placeholders for HumanTasks).
	 * Drives the "Read" chips next to each input group in the step drawer.
	 */
	borrowed_paths?: Record<string, string[]>;
}

export type InterfaceRegistry = Record<string, NodeInterface>;

/** Coerce the `unknown` `interface_json` from the API into a strongly typed
 *  registry. Tolerant: a non-object payload yields an empty registry rather
 *  than throwing. Per-node parsing is structural — fields the renderer
 *  doesn't need stay typed as `unknown` (caller never touches them). */
export function parseInterfaceRegistry(raw: unknown): InterfaceRegistry {
	if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return {};
	return raw as InterfaceRegistry;
}
