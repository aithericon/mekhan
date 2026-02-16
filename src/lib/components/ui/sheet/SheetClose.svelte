<script lang="ts">
	import type { Snippet } from 'svelte';
	import { Dialog } from 'bits-ui';

	type Props = { children?: Snippet; class?: string };
	let { children, class: className }: Props = $props();
</script>

<Dialog.Close
	class="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground {className ?? ''}"
>
	{@render children?.()}
</Dialog.Close>
