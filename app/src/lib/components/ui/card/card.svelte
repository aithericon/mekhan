<script lang="ts" module>
	import { type VariantProps, tv } from "tailwind-variants";

	export const cardVariants = tv({
		base: "text-card-foreground",
		variants: {
			tone: {
				default: "bg-card border rounded-xl shadow-xs flex flex-col gap-4 py-4",
				muted: "bg-muted/50 rounded-lg p-3",
				inset: "bg-card rounded-lg p-3",
				// Inverted khaki callout — at most one per view; a counterweight
				// to the blue primary, not a second interactive surface.
				warm: "bg-accent-warm text-accent-warm-foreground border border-transparent rounded-xl shadow-xs flex flex-col gap-4 py-4 [&_[data-slot=card-description]]:text-accent-warm-foreground/75",
			},
		},
		defaultVariants: {
			tone: "default",
		},
	});

	export type CardTone = VariantProps<typeof cardVariants>["tone"];
</script>

<script lang="ts">
	import type { HTMLAttributes } from "svelte/elements";
	import { cn, type WithElementRef } from "$lib/utils.js";

	let {
		ref = $bindable(null),
		class: className,
		tone = "default",
		children,
		...restProps
	}: WithElementRef<HTMLAttributes<HTMLDivElement>> & {
		tone?: CardTone;
	} = $props();
</script>

<div
	bind:this={ref}
	data-slot="card"
	class={cn(cardVariants({ tone }), className)}
	{...restProps}
>
	{@render children?.()}
</div>
