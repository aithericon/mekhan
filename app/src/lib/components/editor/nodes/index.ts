import type { Component } from 'svelte';
import type { NodeTypes } from '@xyflow/svelte';
import type { NodeKind } from '$lib/types/editor';
import StartNode from './StartNode.svelte';
import EndNode from './EndNode.svelte';
import HumanTaskNode from './HumanTaskNode.svelte';
import AutomatedStepNode from './AutomatedStepNode.svelte';
import DecisionNode from './DecisionNode.svelte';
import ParallelSplitNode from './ParallelSplitNode.svelte';
import JoinNode from './JoinNode.svelte';
import LoopNode from './LoopNode.svelte';
import MapNode from './MapNode.svelte';
import ScopeNode from './ScopeNode.svelte';
import PhaseUpdateNode from './PhaseUpdateNode.svelte';
import ProgressUpdateNode from './ProgressUpdateNode.svelte';
import FailureNode from './FailureNode.svelte';
import DelayNode from './DelayNode.svelte';
import TimeoutNode from './TimeoutNode.svelte';
import TriggerNode from './TriggerNode.svelte';
import SubWorkflowNode from './SubWorkflowNode.svelte';
import AgentNode from './AgentNode.svelte';
import StreamConsumerNode from './StreamConsumerNode.svelte';

// Compile-time exhaustiveness: `satisfies Record<NodeKind, Component>` makes a
// missing or spurious node kind a build error (NodeKind is the canonical
// 18-kind union derived from the generated OpenAPI schema). The trailing
// `as NodeTypes` cast hands xyflow its own (looser, internal-node-typed)
// registry shape without losing the key-level exhaustiveness check.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const nodeTypes = {
	start: StartNode,
	end: EndNode,
	human_task: HumanTaskNode,
	automated_step: AutomatedStepNode,
	decision: DecisionNode,
	parallel_split: ParallelSplitNode,
	join: JoinNode,
	loop: LoopNode,
	map: MapNode,
	scope: ScopeNode,
	phase_update: PhaseUpdateNode,
	progress_update: ProgressUpdateNode,
	failure: FailureNode,
	delay: DelayNode,
	timeout: TimeoutNode,
	trigger: TriggerNode,
	sub_workflow: SubWorkflowNode,
	agent: AgentNode,
	stream_consumer: StreamConsumerNode
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
} satisfies Record<NodeKind, Component<any>> as unknown as NodeTypes;
