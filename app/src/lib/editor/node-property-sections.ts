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
import LeaseScopeNodeSection from '$lib/components/editor/panels/property-sections/LeaseScopeNodeSection.svelte';
import TriggerNodeSection from '$lib/components/editor/panels/property-sections/TriggerNodeSection.svelte';
import SubWorkflowSection from '$lib/components/editor/panels/property-sections/SubWorkflowSection.svelte';
import AgentNodeSection from '$lib/components/editor/panels/property-sections/AgentNodeSection.svelte';
import StreamFoldSection from '$lib/components/editor/panels/property-sections/StreamFoldSection.svelte';
// Config-spec layer migrations: start, end, map, phase_update, progress_update,
// failure, delay, timeout are driven by spec wrappers. Their bespoke section
// files still exist; these wrappers replace the registry entries so the
// satisfies-check continues to pass over all NodeKind values.
import StartSpecSection from '$lib/components/editor/panels/config-spec/StartSpecSection.svelte';
import EndSpecSection from '$lib/components/editor/panels/config-spec/EndSpecSection.svelte';
import MapSpecSection from '$lib/components/editor/panels/config-spec/MapSpecSection.svelte';
import PhaseUpdateSpecSection from '$lib/components/editor/panels/config-spec/PhaseUpdateSpecSection.svelte';
import ProgressUpdateSpecSection from '$lib/components/editor/panels/config-spec/ProgressUpdateSpecSection.svelte';
import FailureSpecSection from '$lib/components/editor/panels/config-spec/FailureSpecSection.svelte';
import DelaySpecSection from '$lib/components/editor/panels/config-spec/DelaySpecSection.svelte';
import TimeoutSpecSection from '$lib/components/editor/panels/config-spec/TimeoutSpecSection.svelte';

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
	lease_scope: LeaseScopeNodeSection,
	phase_update: PhaseUpdateSpecSection,
	progress_update: ProgressUpdateSpecSection,
	failure: FailureSpecSection,
	delay: DelaySpecSection,
	timeout: TimeoutSpecSection,
	trigger: TriggerNodeSection,
	sub_workflow: SubWorkflowSection,
	agent: AgentNodeSection,
	stream_fold: StreamFoldSection
} satisfies Record<NodeKind, Component<any>>;

/**
 * The uniform prop contract every section receives. Sections destructure
 * only what they need.
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

export const NODE_PROPERTY_SECTIONS: Record<NodeKind, Component<SectionProps>> =
	SECTIONS as Record<NodeKind, Component<SectionProps>>;
