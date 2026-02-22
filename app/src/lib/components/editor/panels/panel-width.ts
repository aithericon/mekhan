import type { WorkflowNodeData } from '$lib/types/editor';

/**
 * Returns the Tailwind width class for the expanded sheet panel
 * based on the complexity of the selected node's config.
 */
export function getSheetWidthClass(data: WorkflowNodeData): string {
	switch (data.type) {
		case 'human_task':
			return 'w-[640px]';

		case 'automated_step': {
			const bt = data.executionSpec.backendType;
			if (bt === 'python' || bt === 'llm' || bt === 'http') return 'w-[60vw]';
			return 'w-[480px]';
		}

		case 'decision':
			return 'w-[480px]';

		default:
			return 'w-[480px]';
	}
}
