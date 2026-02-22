<script lang="ts">
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { Input } from '$lib/components/ui/input';
	import { Button } from '$lib/components/ui/button';

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

<div class="space-y-2">
	{#each items as item, i (i)}
		<div class="flex items-center gap-1.5">
			<Input
				type="text"
				value={item}
				{placeholder}
				disabled={readonly}
				oninput={(e) => updateItem(i, (e.currentTarget as HTMLInputElement).value)}
				class="flex-1"
			/>
			{#if !readonly}
				<button
					type="button"
					class="rounded p-1 text-muted-foreground transition-colors hover:text-destructive"
					onclick={() => removeItem(i)}
				>
					<Trash2 class="size-4" />
				</button>
			{/if}
		</div>
	{/each}

	{#if !readonly}
		<button
			type="button"
			class="flex w-full items-center justify-center gap-1.5 rounded-md border border-dashed border-border py-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
			onclick={addItem}
		>
			<Plus class="size-4" />
			Add Item
		</button>
	{/if}

	{#if items.length === 0 && readonly}
		<p class="text-sm italic text-muted-foreground">No items</p>
	{/if}
</div>
