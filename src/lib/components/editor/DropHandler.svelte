<script lang="ts">
	import { useSvelteFlow, type XYPosition } from '@xyflow/svelte';

	type Props = {
		oninit: (helpers: { screenToFlowPosition: (pos: { x: number; y: number }) => XYPosition }) => void;
	};

	let { oninit }: Props = $props();

	const { screenToFlowPosition } = useSvelteFlow();

	// Expose the helper to the parent immediately
	oninit({ screenToFlowPosition });
</script>
