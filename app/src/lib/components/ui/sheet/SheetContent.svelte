<script lang="ts">
	import type { Snippet } from 'svelte';
	import { Dialog } from 'bits-ui';
	import { cn } from '$lib/utils.js';

	type Props = {
		children?: Snippet;
		class?: string;
		onInteractOutside?: (e: Event) => void;
	};

	let { children, class: className, onInteractOutside }: Props = $props();
</script>

<Dialog.Portal>
	<Dialog.Overlay class="fixed inset-0 z-50" />
	<Dialog.Content
		preventScroll={false}
		{onInteractOutside}
		class={cn("fixed inset-y-0 right-0 z-50 flex flex-col border-l border-border bg-card shadow-lg duration-200 ease-in-out data-[state=open]:animate-in data-[state=open]:slide-in-from-right data-[state=closed]:animate-out data-[state=closed]:slide-out-to-right", className)}
	>
		{@render children?.()}
	</Dialog.Content>
</Dialog.Portal>
