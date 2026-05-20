<script lang="ts">
	import Trash2 from '@lucide/svelte/icons/trash-2';

	type Props = {
		readonly?: boolean;
		onremove: () => void;
	};

	let { readonly = false, onremove }: Props = $props();
</script>

<div
	class="flex items-center gap-2 rounded-md border border-border/50 bg-background px-3 py-2"
>
	<hr class="flex-1 border-border" />
	<span class="text-sm text-muted-foreground">Divider</span>
	<hr class="flex-1 border-border" />
	{#if !readonly}
		<button
			type="button"
			class="rounded p-1 text-muted-foreground transition-colors hover:text-destructive"
			onclick={onremove}
		>
			<Trash2 class="size-4" />
		</button>
	{/if}
</div>
