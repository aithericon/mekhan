<script lang="ts">
	import Trash2 from '@lucide/svelte/icons/trash-2';

	type Props = {
		readonly?: boolean;
		onremove: () => void;
	};

	let { readonly = false, onremove }: Props = $props();
</script>

<div
	class="flex items-center gap-2 rounded border border-border/50 bg-background px-2 py-1.5"
>
	<hr class="flex-1 border-border" />
	<span class="text-[9px] text-muted-foreground">Divider</span>
	<hr class="flex-1 border-border" />
	{#if !readonly}
		<button
			type="button"
			class="rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
			onclick={onremove}
		>
			<Trash2 class="size-3" />
		</button>
	{/if}
</div>
