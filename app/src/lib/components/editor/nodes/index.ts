import type { NodeTypes } from '@xyflow/svelte';
import StartNode from './StartNode.svelte';
import EndNode from './EndNode.svelte';
import HumanTaskNode from './HumanTaskNode.svelte';
import AutomatedStepNode from './AutomatedStepNode.svelte';
import DecisionNode from './DecisionNode.svelte';
import ParallelSplitNode from './ParallelSplitNode.svelte';
import ParallelJoinNode from './ParallelJoinNode.svelte';
import JoinNode from './JoinNode.svelte';
import LoopNode from './LoopNode.svelte';
import ScopeNode from './ScopeNode.svelte';
import PhaseUpdateNode from './PhaseUpdateNode.svelte';
import ProgressUpdateNode from './ProgressUpdateNode.svelte';
import FailureNode from './FailureNode.svelte';
import TriggerNode from './TriggerNode.svelte';
import SubWorkflowNode from './SubWorkflowNode.svelte';

export const nodeTypes: NodeTypes = {
	start: StartNode as any,
	end: EndNode as any,
	human_task: HumanTaskNode as any,
	automated_step: AutomatedStepNode as any,
	decision: DecisionNode as any,
	parallel_split: ParallelSplitNode as any,
	parallel_join: ParallelJoinNode as any,
	join: JoinNode as any,
	loop: LoopNode as any,
	scope: ScopeNode as any,
	phase_update: PhaseUpdateNode as any,
	progress_update: ProgressUpdateNode as any,
	failure: FailureNode as any,
	trigger: TriggerNode as any,
	sub_workflow: SubWorkflowNode as any
};
