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

import StartNodeSection from '$lib/components/editor/panels/property-sections/StartNodeSection.svelte';
import HumanTaskSectionWrapper from '$lib/components/editor/panels/property-sections/HumanTaskSectionWrapper.svelte';
import AutomatedStepSectionWrapper from '$lib/components/editor/panels/property-sections/AutomatedStepSectionWrapper.svelte';
import DecisionNodeSection from '$lib/components/editor/panels/property-sections/DecisionNodeSection.svelte';
import ParallelSplitSection from '$lib/components/editor/panels/property-sections/ParallelSplitSection.svelte';
import JoinSection from '$lib/components/editor/panels/property-sections/JoinSection.svelte';
import LoopNodeSection from '$lib/components/editor/panels/property-sections/LoopNodeSection.svelte';
import ScopeSection from '$lib/components/editor/panels/property-sections/ScopeSection.svelte';
import PhaseUpdateNodeSection from '$lib/components/editor/panels/property-sections/PhaseUpdateNodeSection.svelte';
import ProgressUpdateNodeSection from '$lib/components/editor/panels/property-sections/ProgressUpdateNodeSection.svelte';
import FailureNodeSection from '$lib/components/editor/panels/property-sections/FailureNodeSection.svelte';
import DelayNodeSection from '$lib/components/editor/panels/property-sections/DelayNodeSection.svelte';
import TimeoutNodeSection from '$lib/components/editor/panels/property-sections/TimeoutNodeSection.svelte';
import TriggerNodeSection from '$lib/components/editor/panels/property-sections/TriggerNodeSection.svelte';
import SubWorkflowSection from '$lib/components/editor/panels/property-sections/SubWorkflowSection.svelte';
import AgentNodeSection from '$lib/components/editor/panels/property-sections/AgentNodeSection.svelte';
import EndNodeSection from '$lib/components/editor/panels/property-sections/EndNodeSection.svelte';

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
	start: StartNodeSection,
	end: EndNodeSection,
	human_task: HumanTaskSectionWrapper,
	automated_step: AutomatedStepSectionWrapper,
	decision: DecisionNodeSection,
	parallel_split: ParallelSplitSection,
	join: JoinSection,
	loop: LoopNodeSection,
	scope: ScopeSection,
	phase_update: PhaseUpdateNodeSection,
	progress_update: ProgressUpdateNodeSection,
	failure: FailureNodeSection,
	delay: DelayNodeSection,
	timeout: TimeoutNodeSection,
	trigger: TriggerNodeSection,
	sub_workflow: SubWorkflowSection,
	agent: AgentNodeSection
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
} satisfies Record<NodeKind, Component<any>>;

export const NODE_PROPERTY_SECTIONS: Record<NodeKind, Component<SectionProps>> =
	SECTIONS as unknown as Record<NodeKind, Component<SectionProps>>;
