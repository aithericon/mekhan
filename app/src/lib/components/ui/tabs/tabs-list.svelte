<script lang="ts" module>
	// SPDX-License-Identifier: Apache-2.0
	import { type VariantProps, tv } from 'tailwind-variants';

	export const tabsListVariants = tv({
		base: '',
		variants: {
			variant: {
				// The stock shadcn pill list — component-level tabs (cards, panels,
				// drawers). Unchanged.
				default:
					'bg-muted text-muted-foreground inline-flex h-9 w-fit items-center justify-center rounded-lg p-[3px]',
				// GitHub-style underline tabs for PAGE-LEVEL state tabs rendered in
				// the PageShell header band (`tabs` snippet): quiet labels with a
				// 2px active underline that sits flush on the band's border-b.
				// Pair with variant="underline" on each Tabs.Trigger.
				underline: 'text-muted-foreground flex w-fit items-end gap-1'
			}
		},
		defaultVariants: { variant: 'default' }
	});

	export type TabsListVariant = VariantProps<typeof tabsListVariants>['variant'];
</script>

<script lang="ts">
	import { Tabs as TabsPrimitive } from 'bits-ui';
	import { cn } from '$lib/utils.js';

	let {
		ref = $bindable(null),
		class: className,
		variant = 'default',
		...restProps
	}: TabsPrimitive.ListProps & { variant?: TabsListVariant } = $props();
</script>

<TabsPrimitive.List
	bind:ref
	data-slot="tabs-list"
	class={cn(tabsListVariants({ variant }), className)}
	{...restProps}
/>
