<script lang="ts">
	import { cn } from '$lib/utils';
	import { useStepperIndicator } from '$lib/components/ui/stepper/stepper.svelte.js';
	import type { HTMLAttributes } from 'svelte/elements';

	let { class: className, children, ...rest }: HTMLAttributes<HTMLDivElement> = $props();

	const itemState = useStepperIndicator();
</script>

<div
	data-slot="stepper-indicator"
	class={cn(
		'z-1 flex size-7 shrink-0 items-center justify-center rounded-full ring-2 ring-background transition-all select-none [&_svg]:size-4',
		itemState.state === 'inactive'
			? 'bg-muted text-muted-foreground'
			: 'bg-primary text-primary-foreground',
		itemState.isCurrent && 'ring-6 ring-primary/25',
		className
	)}
	{...rest}
>
	{@render children?.()}
</div>
