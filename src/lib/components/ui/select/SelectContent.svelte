<script lang="ts">
	import { Select, type SelectContentProps } from 'bits-ui';
	import { cn } from '$lib/utils.js';

	let {
		children,
		class: className,
		sideOffset = 4,
		...restProps
	}: SelectContentProps & { class?: string } = $props();
</script>

<Select.Portal>
	<Select.Content
		{sideOffset}
		class={cn("z-50 min-w-[8rem] overflow-hidden rounded-md border border-border bg-popover text-popover-foreground shadow-md animate-in fade-in-0 zoom-in-95", className)}
		{...restProps}
	>
		<Select.Viewport class="p-1">
			{@render children?.()}
		</Select.Viewport>
	</Select.Content>
</Select.Portal>
