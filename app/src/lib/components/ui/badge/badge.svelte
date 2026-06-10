<script lang="ts" module>
	import { type VariantProps, tv } from "tailwind-variants";

	export const badgeVariants = tv({
		base: "focus-visible:border-ring focus-visible:ring-ring/50 aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 aria-invalid:border-destructive inline-flex w-fit shrink-0 items-center justify-center gap-1 overflow-hidden rounded-full border font-medium whitespace-nowrap transition-[color,box-shadow] focus-visible:ring-[3px] [&>svg]:pointer-events-none",
		variants: {
			variant: {
				default:
					"bg-primary text-primary-foreground [a&]:hover:bg-primary/90 border-transparent",
				secondary:
					"bg-secondary text-secondary-foreground [a&]:hover:bg-secondary/90 border-transparent",
				destructive:
					"bg-destructive [a&]:hover:bg-destructive/90 focus-visible:ring-destructive/20 dark:focus-visible:ring-destructive/40 dark:bg-destructive/70 border-transparent text-white",
				outline: "text-foreground [a&]:hover:bg-accent [a&]:hover:text-accent-foreground",
				success:
					"bg-success/15 text-success [a&]:hover:bg-success/25 border-transparent",
				warning:
					"bg-warning/20 text-warning-foreground [a&]:hover:bg-warning/30 border-transparent",
				info:
					"bg-info/15 text-info [a&]:hover:bg-info/25 border-transparent",
				muted:
					"bg-muted text-muted-foreground [a&]:hover:bg-muted/80 border-transparent",
				warm:
					"bg-accent-warm text-accent-warm-foreground [a&]:hover:bg-accent-warm/90 border-transparent",
			},
			size: {
				sm: "px-2 py-0.5 text-xs [&>svg]:size-3",
				xs: "px-1.5 py-0 text-[10px] [&>svg]:size-2.5",
			},
		},
		defaultVariants: {
			variant: "default",
			size: "sm",
		},
	});

	export type BadgeVariant = VariantProps<typeof badgeVariants>["variant"];
	export type BadgeSize = VariantProps<typeof badgeVariants>["size"];
</script>

<script lang="ts">
	import type { HTMLAnchorAttributes } from "svelte/elements";
	import { cn, type WithElementRef } from "$lib/utils.js";

	let {
		ref = $bindable(null),
		href,
		class: className,
		variant = "default",
		size = "sm",
		children,
		...restProps
	}: WithElementRef<HTMLAnchorAttributes> & {
		variant?: BadgeVariant;
		size?: BadgeSize;
	} = $props();
</script>

<svelte:element
	this={href ? "a" : "span"}
	bind:this={ref}
	data-slot="badge"
	{href}
	class={cn(badgeVariants({ variant, size }), className)}
	{...restProps}
>
	{@render children?.()}
</svelte:element>
