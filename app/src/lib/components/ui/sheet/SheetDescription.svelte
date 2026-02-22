<script lang="ts">
	import type { Snippet } from 'svelte';
	import { Dialog } from 'bits-ui';

	type Props = { children?: Snippet; class?: string };
	let { children, class: className }: Props = $props();
</script>

<Dialog.Description class="sr-only {className ?? ''}">
	{@render children?.()}
</Dialog.Description>
