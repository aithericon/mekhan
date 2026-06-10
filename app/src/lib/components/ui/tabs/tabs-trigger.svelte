<script lang="ts" module>
	// SPDX-License-Identifier: Apache-2.0
	import { type VariantProps, tv } from 'tailwind-variants';

	export const tabsTriggerVariants = tv({
		base: '',
		variants: {
			variant: {
				// The stock shadcn pill trigger — component-level tabs. Unchanged.
				default:
					"data-[state=active]:bg-background dark:data-[state=active]:text-foreground focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:outline-ring dark:data-[state=active]:border-input dark:data-[state=active]:bg-input/30 text-foreground dark:text-muted-foreground inline-flex h-[calc(100%-1px)] flex-1 items-center justify-center gap-1.5 rounded-md border border-transparent px-2 py-1 text-sm font-medium whitespace-nowrap transition-[color,box-shadow] focus-visible:ring-[3px] focus-visible:outline-1 disabled:pointer-events-none disabled:opacity-50 data-[state=active]:shadow-sm [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4",
				// GitHub-style underline trigger for PAGE-LEVEL state tabs in the
				// PageShell band. Kept pixel-compatible with shell/PageTabs (link
				// tabs) so state tabs and link tabs look identical. No background,
				// no radius — a 2px active underline meant to overlap the band's
				// border-b (PageShell pulls the tab row down with -mb-px).
				underline:
					"focus-visible:ring-ring/50 focus-visible:outline-ring inline-flex items-center justify-center gap-1.5 border-b-2 border-transparent px-3 py-2 text-sm font-medium whitespace-nowrap text-muted-foreground transition-colors hover:border-border hover:text-foreground focus-visible:ring-[3px] focus-visible:outline-1 disabled:pointer-events-none disabled:opacity-50 data-[state=active]:border-primary data-[state=active]:text-foreground [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4"
			}
		},
		defaultVariants: { variant: 'default' }
	});

	export type TabsTriggerVariant = VariantProps<typeof tabsTriggerVariants>['variant'];
</script>

<script lang="ts">
	import { Tabs as TabsPrimitive } from 'bits-ui';
	import { cn } from '$lib/utils.js';

	let {
		ref = $bindable(null),
		class: className,
		variant = 'default',
		...restProps
	}: TabsPrimitive.TriggerProps & { variant?: TabsTriggerVariant } = $props();
</script>

<TabsPrimitive.Trigger
	bind:ref
	data-slot="tabs-trigger"
	class={cn(tabsTriggerVariants({ variant }), className)}
	{...restProps}
/>
