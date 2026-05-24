<script lang="ts">
	import { cn } from '$lib/utils';
	import { useStepperNav } from '$lib/components/ui/stepper/stepper.svelte.js';
	import type { StepperNavProps } from '$lib/components/ui/stepper/types';
	import { box } from 'svelte-toolbelt';

	let {
		orientation = 'horizontal',
		class: className,
		children,
		...rest
	}: StepperNavProps = $props();

	const stepperNavState = useStepperNav({
		orientation: box.with(() => orientation)
	});

	let navRef = $state<HTMLDivElement | null>(null);

	$effect(() => {
		const step = stepperNavState.currentStep;
		const el = navRef;
		if (!el || orientation !== 'horizontal') return;

		const items = el.querySelectorAll<HTMLElement>('[data-slot="stepper-item"]');
		const activeItem = items[step - 1];
		if (!activeItem) return;

		const containerRect = el.getBoundingClientRect();
		const scrollLeft =
			activeItem.offsetLeft - containerRect.width / 2 + activeItem.offsetWidth / 2;

		el.scrollTo({ left: scrollLeft, behavior: 'smooth' });
	});
</script>

<div
	bind:this={navRef}
	data-slot="stepper-nav"
	class={cn(
		'group/stepper-nav flex',
		{
			'flex-row justify-between': orientation === 'horizontal',
			'flex-col gap-2': orientation === 'vertical'
		},
		orientation === 'horizontal' && [
			'max-md:flex-nowrap',
			'max-md:overflow-x-auto',
			'max-md:-my-2 max-md:py-2',
			'max-md:[scrollbar-width:none]',
			'max-md:[&::-webkit-scrollbar]:hidden'
		],
		className
	)}
	{...stepperNavState.props}
	{...rest}
>
	{#if orientation === 'horizontal'}
		<div class="hidden max-md:block max-md:min-w-[40%] max-md:shrink-0" aria-hidden="true"></div>
	{/if}
	{@render children?.()}
	{#if orientation === 'horizontal'}
		<div class="hidden max-md:block max-md:min-w-[40%] max-md:shrink-0" aria-hidden="true"></div>
	{/if}
</div>
