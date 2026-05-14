<script lang="ts" module>
	import { type VariantProps, tv } from "tailwind-variants";

	export const cardVariants = tv({
		base: "text-card-foreground flex flex-col gap-4 py-4",
		variants: {
			tone: {
				default: "bg-card border rounded-xl shadow-xs",
				muted: "bg-muted/50 rounded-lg",
				inset: "bg-card rounded-lg",
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
