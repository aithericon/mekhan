<script lang="ts">
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';

	type Props = {
		entries: Record<string, unknown>;
		readonly?: boolean;
		keyPlaceholder?: string;
		valuePlaceholder?: string;
		onchange: (entries: Record<string, unknown>) => void;
	};

	let {
		entries,
		readonly = false,
		keyPlaceholder = 'Key',
		valuePlaceholder = 'Value',
		onchange
	}: Props = $props();

	// Convert record to array for rendering
	const rows = $derived(
		Object.entries(entries).map(([key, val]) => ({
			key,
			value: typeof val === 'string' ? val : JSON.stringify(val)
		}))
	);

	function emit(updatedRows: Array<{ key: string; value: string }>) {
		const result: Record<string, unknown> = {};
		for (const row of updatedRows) {
			if (!row.key) continue;
			// Try to parse JSON values back
			try {
				result[row.key] = JSON.parse(row.value);
			} catch {
				result[row.key] = row.value;
			}
		}
		onchange(result);
	}

	function updateKey(index: number, newKey: string) {
		const updated = rows.map((r, i) => (i === index ? { ...r, key: newKey } : { ...r }));
		emit(updated);
	}

	function updateValue(index: number, newValue: string) {
		const updated = rows.map((r, i) => (i === index ? { ...r, value: newValue } : { ...r }));
		emit(updated);
	}

	function addRow() {
		const updated = [...rows.map((r) => ({ ...r })), { key: '', value: '' }];
		emit(updated);
	}

	function removeRow(index: number) {
		const updated = rows.filter((_, i) => i !== index).map((r) => ({ ...r }));
		emit(updated);
	}
</script>

<div class="space-y-1.5">
	{#each rows as row, i (i)}
		<div class="flex items-center gap-1">
			<input
				type="text"
				value={row.key}
				placeholder={keyPlaceholder}
				disabled={readonly}
				oninput={(e) => updateKey(i, (e.currentTarget as HTMLInputElement).value)}
				class="flex-1 rounded border border-input bg-background px-1.5 py-0.5 text-[10px] text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
			/>
			<input
				type="text"
				value={row.value}
				placeholder={valuePlaceholder}
				disabled={readonly}
				oninput={(e) => updateValue(i, (e.currentTarget as HTMLInputElement).value)}
				class="flex-1 rounded border border-input bg-background px-1.5 py-0.5 text-[10px] text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
			/>
			{#if !readonly}
				<button
					type="button"
					class="rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
					onclick={() => removeRow(i)}
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
			onclick={addRow}
		>
			<Plus class="size-3" />
			Add Entry
		</button>
	{/if}

	{#if rows.length === 0 && readonly}
		<p class="text-[10px] italic text-muted-foreground">No entries</p>
	{/if}
</div>
