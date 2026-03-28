<script lang="ts">
	import { useSvelteFlow } from '@xyflow/svelte';
	import type { EventSpotlight } from '$lib/types/petri';

	interface Props {
		spotlight: EventSpotlight | null;
	}

	let { spotlight }: Props = $props();
	const { fitView, getNodes } = useSvelteFlow();

	let lastPannedKey: string | null = null;

	// When spotlight changes to a genuinely new selection, fit viewport once
	$effect(() => {
		if (spotlight && spotlight.allNodeIds.length > 0) {
			const key = [...spotlight.allNodeIds].sort().join(',');
			if (key === lastPannedKey) return;
			lastPannedKey = key;

			const prefixedIds = new Set<string>();
			for (const id of spotlight.allNodeIds) {
				if (id === spotlight.transitionId) {
					prefixedIds.add(`t:${id}`);
				} else {
					prefixedIds.add(`p:${id}`);
				}
			}
			const targetNodes = getNodes().filter(n => prefixedIds.has(n.id));
			if (targetNodes.length > 0) {
				requestAnimationFrame(() => {
					fitView({ nodes: targetNodes, duration: 150, padding: 0.3 });
				});
			}
		} else {
			lastPannedKey = null;
		}
	});
</script>
