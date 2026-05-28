<script lang="ts">
	import { cn } from '$lib/utils';
	import type { HTMLAttributes } from 'svelte/elements';
	import { useStepperSeparator } from '$lib/components/ui/stepper/stepper.svelte.js';

	let { class: className, children, ...rest }: HTMLAttributes<HTMLDivElement> = $props();

	const separatorState = useStepperSeparator();
</script>

<div
	data-slot="stepper-separator"
	class={cn(
		'absolute left-0 shrink-0 bg-border/80 transition-colors data-[current=true]:bg-primary/45 data-[state=completed]:bg-primary',
		'group-data-[orientation=horizontal]/stepper-nav:top-[12px] group-data-[orientation=horizontal]/stepper-nav:h-1 group-data-[orientation=horizontal]/stepper-nav:w-full',
		'group-data-[orientation=vertical]/stepper-nav:top-[28px] group-data-[orientation=vertical]/stepper-nav:left-[12px] group-data-[orientation=vertical]/stepper-nav:h-full group-data-[orientation=vertical]/stepper-nav:w-1',
		className
	)}
	{...separatorState.props}
	{...rest}
>
	{@render children?.()}
</div>
