import type { NodeTypes } from '@xyflow/svelte';
import StartNode from './StartNode.svelte';
import EndNode from './EndNode.svelte';
import HumanTaskNode from './HumanTaskNode.svelte';
import AutomatedStepNode from './AutomatedStepNode.svelte';
import DecisionNode from './DecisionNode.svelte';
import ParallelSplitNode from './ParallelSplitNode.svelte';
import ParallelJoinNode from './ParallelJoinNode.svelte';
import LoopNode from './LoopNode.svelte';

export const nodeTypes: NodeTypes = {
	start: StartNode as any,
	end: EndNode as any,
	human_task: HumanTaskNode as any,
	automated_step: AutomatedStepNode as any,
	decision: DecisionNode as any,
	parallel_split: ParallelSplitNode as any,
	parallel_join: ParallelJoinNode as any,
	loop: LoopNode as any
};
