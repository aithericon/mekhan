<script lang="ts">
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';

	type Props = {
		items: string[];
		readonly?: boolean;
		placeholder?: string;
		onchange: (items: string[]) => void;
	};

	let { items, readonly = false, placeholder = 'Value', onchange }: Props = $props();

	function updateItem(index: number, value: string) {
		const updated = [...items];
		updated[index] = value;
		onchange(updated);
	}

	function addItem() {
		onchange([...items, '']);
	}

	function removeItem(index: number) {
		onchange(items.filter((_, i) => i !== index));
	}
</script>

<div class="space-y-1.5">
	{#each items as item, i (i)}
		<div class="flex items-center gap-1">
			<input
				type="text"
				value={item}
				{placeholder}
				disabled={readonly}
				oninput={(e) => updateItem(i, (e.currentTarget as HTMLInputElement).value)}
				class="flex-1 rounded border border-input bg-background px-1.5 py-0.5 text-[10px] text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
			/>
			{#if !readonly}
				<button
					type="button"
					class="rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
					onclick={() => removeItem(i)}
				>
					<Trash2 class="size-3" />
				</button>
			{/if}
		</div>
	{/each}

	{#if !readonly}
		<button
			type="button"
			class="flex w-full items-center justify-center gap-1 rounded border border-dashed border-border py-1 text-[10px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
			onclick={addItem}
		>
			<Plus class="size-3" />
			Add Item
		</button>
	{/if}

	{#if items.length === 0 && readonly}
		<p class="text-[10px] italic text-muted-foreground">No items</p>
	{/if}
</div>
