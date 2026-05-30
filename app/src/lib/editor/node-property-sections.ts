/**
 * Map from workflow node kind to the Svelte component that renders its
 * property-panel section. Pairs with the node-renderer registry in
 * `components/editor/nodes/index.ts` and mirrors the backend-panel registry
 * in `backend-panels.ts`.
 *
 * Compile-time exhaustiveness via `satisfies Record<NodeKind, …>` makes
 * "added a node kind but forgot its section" (or a spurious kind) a build
 * error — `NodeKind` is the canonical 17-kind union derived from the
 * generated OpenAPI schema.
 *
 * Every section is dispatched with the SAME superset of props (`SectionProps`)
 * from NodePropertyPanel via `<svelte:component this={SECTIONS[data.type]} … />`.
 * Sections destructure only the props they need and ignore the rest. The two
 * non-uniform cases (`human_task`'s template-vs-inline branch and
 * `automated_step`'s config + retry-policy pair) are folded into thin wrapper
 * components so the value type stays a flat `Record<NodeKind, Component>`.
 */

import type { Component } from 'svelte';
import type { NodeKind, WorkflowNodeData } from '$lib/types/editor';
import type { ScopeEntry } from '$lib/editor/guard-scope';
import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

import HumanTaskSectionWrapper from '$lib/components/editor/panels/property-sections/HumanTaskSectionWrapper.svelte';
import AutomatedStepSectionWrapper from '$lib/components/editor/panels/property-sections/AutomatedStepSectionWrapper.svelte';
import DecisionNodeSection from '$lib/components/editor/panels/property-sections/DecisionNodeSection.svelte';
import ParallelSplitSection from '$lib/components/editor/panels/property-sections/ParallelSplitSection.svelte';
import JoinSection from '$lib/components/editor/panels/property-sections/JoinSection.svelte';
import LoopNodeSection from '$lib/components/editor/panels/property-sections/LoopNodeSection.svelte';
import ScopeSection from '$lib/components/editor/panels/property-sections/ScopeSection.svelte';
import TriggerNodeSection from '$lib/components/editor/panels/property-sections/TriggerNodeSection.svelte';
import SubWorkflowSection from '$lib/components/editor/panels/property-sections/SubWorkflowSection.svelte';
import AgentNodeSection from '$lib/components/editor/panels/property-sections/AgentNodeSection.svelte';
// Tier-1 spec-layer migrations: delay, progress_update, timeout, phase_update, map, failure.
// Tier-2 spec-layer migrations: start, end.
// Bespoke section files still exist; these wrappers replace their registry entries
// so the satisfies-check continues to pass over all NodeKind values.
import ProgressUpdateSpecSection from '$lib/components/editor/panels/config-spec/ProgressUpdateSpecSection.svelte';
import DelaySpecSection from '$lib/components/editor/panels/config-spec/DelaySpecSection.svelte';
import TimeoutSpecSection from '$lib/components/editor/panels/config-spec/TimeoutSpecSection.svelte';
import PhaseUpdateSpecSection from '$lib/components/editor/panels/config-spec/PhaseUpdateSpecSection.svelte';
import MapSpecSection from '$lib/components/editor/panels/config-spec/MapSpecSection.svelte';
import FailureSpecSection from '$lib/components/editor/panels/config-spec/FailureSpecSection.svelte';
import EndSpecSection from '$lib/components/editor/panels/config-spec/EndSpecSection.svelte';
import StartSpecSection from '$lib/components/editor/panels/config-spec/StartSpecSection.svelte';

/**
 * The uniform prop contract NodePropertyPanel dispatches every section with.
 * Individual sections accept a subset; extras are ignored. `data` is the broad
 * union here — each section narrows it via its own `Props` type. Section
 * components have contravariant (narrower-`data`) prop types, so the registry's
 * value type is `Component<any>` (exhaustiveness still holds via the
 * `satisfies Record<NodeKind, …>` key check); the caller annotates the spread
 * payload as `SectionProps` so the dispatch site is type-checked.
 */
export type SectionProps = {
	data: WorkflowNodeData;
	readonly: boolean;
	onchange: (data: WorkflowNodeData) => void;
	binding?: YjsGraphBinding;
	nodeId?: string;
	templateId?: string;
	scope?: ScopeEntry[];
	resourceScope?: ScopeEntry[];
	onselectnode?: (id: string) => void;
};

// The literal is checked with `satisfies Record<NodeKind, Component<any>>` for
// exhaustiveness (missing/spurious kind → compile error). It's then re-typed to
// `Record<NodeKind, Component<SectionProps>>` for the export so the dispatch
// site (`<svelte:component this={…}>`) sees a single uniform component type
// instead of an intersection of every section's narrower Props. The cast is
// sound because every section reads a subset of `SectionProps`.
const SECTIONS = {
	start: StartSpecSection,
	end: EndSpecSection,
	human_task: HumanTaskSectionWrapper,
	automated_step: AutomatedStepSectionWrapper,
	decision: DecisionNodeSection,
	parallel_split: ParallelSplitSection,
	join: JoinSection,
	loop: LoopNodeSection,
	map: MapSpecSection,
	scope: ScopeSection,
	phase_update: PhaseUpdateSpecSection,
	progress_update: ProgressUpdateSpecSection,
	failure: FailureSpecSection,
	delay: DelaySpecSection,
	timeout: TimeoutSpecSection,
	trigger: TriggerNodeSection,
	sub_workflow: SubWorkflowSection,
	agent: AgentNodeSection
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
} satisfies Record<NodeKind, Component<any>>;

export const NODE_PROPERTY_SECTIONS: Record<NodeKind, Component<SectionProps>> =
	SECTIONS as unknown as Record<NodeKind, Component<SectionProps>>;
