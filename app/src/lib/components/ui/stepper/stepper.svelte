<script lang="ts">
	import { useStepperRoot } from '$lib/components/ui/stepper/stepper.svelte.js';
	import type { StepperRootProps } from '$lib/components/ui/stepper/types';
	import { box } from 'svelte-toolbelt';

	let { step = $bindable(1), completed = false, children }: StepperRootProps = $props();

	useStepperRoot({
		step: box.with(
			() => step,
			(v) => (step = v)
		),
		completed: box.with(() => completed)
	});
</script>

{@render children?.()}
