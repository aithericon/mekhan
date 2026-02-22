import type { EdgeTypes } from '@xyflow/svelte';
import DeletableEdge from './DeletableEdge.svelte';

export const edgeTypes: EdgeTypes = {
	deletable: DeletableEdge as any
};
