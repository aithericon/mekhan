<script lang="ts">
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { Input } from '$lib/components/ui/input';
	import InsertRefButton from '../property-sections/InsertRefButton.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';

	type Row = { key: string; value: string };

	type Props = {
		entries: Record<string, unknown>;
		readonly?: boolean;
		keyPlaceholder?: string;
		valuePlaceholder?: string;
		onchange: (entries: Record<string, unknown>) => void;
		/** When provided + non-empty + not readonly, each value row gets an
		 *  InsertRefButton that appends an `{{ <slug>.<field> }}` snippet. */
		scope?: ScopeEntry[];
	};

	let {
		entries,
		readonly = false,
		keyPlaceholder = 'Key',
		valuePlaceholder = 'Value',
		onchange,
		scope = []
	}: Props = $props();

	function toRows(e: Record<string, unknown>): Row[] {
		return Object.entries(e).map(([key, val]) => ({
			key,
			value: typeof val === 'string' ? val : JSON.stringify(val)
		}));
	}

	// Rows are local-authoritative so a freshly-added empty-key row actually
	// renders. The previous all-derived version filtered empties inside
	// emit(), so onchange round-tripped the same `entries` and the new row
	// was invisible — `+ Add Entry` looked dead.
	let rows = $state<Row[]>([]);

	// Resync from `entries` only when the prop changes to something we did
	// NOT just emit (e.g. Yjs remote update, parent reset). `lastEmitted` is
	// the JSON signature we last acknowledged; matching it means the prop
	// change is our own echo and we keep local draft rows (including empties).
	// `null` sentinel forces the first run to seed `rows` from the incoming
	// `entries` without treating it as an external resync.
	let lastEmitted = $state<string | null>(null);
	$effect(() => {
		const incoming = JSON.stringify(entries);
		if (lastEmitted === null) {
			rows = toRows(entries);
			lastEmitted = incoming;
			return;
		}
		if (incoming !== lastEmitted) {
			rows = toRows(entries);
			lastEmitted = incoming;
		}
	});

	function emit() {
		const result: Record<string, unknown> = {};
		for (const row of rows) {
			if (!row.key) continue;
			try {
				result[row.key] = JSON.parse(row.value);
			} catch {
				result[row.key] = row.value;
			}
		}
		lastEmitted = JSON.stringify(result);
		onchange(result);
	}

	function updateKey(index: number, newKey: string) {
		rows[index].key = newKey;
		emit();
	}

	function updateValue(index: number, newValue: string) {
		rows[index].value = newValue;
		emit();
	}

	function addRow() {
		rows = [...rows, { key: '', value: '' }];
		// No emit — an empty-key row contributes nothing to `entries` until
		// the user fills the key. emit() runs on the first keystroke.
	}

	function removeRow(index: number) {
		rows = rows.filter((_, i) => i !== index);
		emit();
	}

	function appendRefToValue(index: number, snippet: string) {
		const curr = rows[index].value;
		rows[index].value = curr ? `${curr}${snippet}` : snippet;
		emit();
	}
</script>

<div class="space-y-1.5">
	{#each rows as row, i (i)}
		<div class="flex items-center gap-1">
			<Input
				type="text"
				value={row.key}
				placeholder={keyPlaceholder}
				disabled={readonly}
				oninput={(e) => updateKey(i, (e.currentTarget as HTMLInputElement).value)}
				class="h-8 flex-1"
			/>
			<Input
				type="text"
				value={row.value}
				placeholder={valuePlaceholder}
				disabled={readonly}
				oninput={(e) => updateValue(i, (e.currentTarget as HTMLInputElement).value)}
				class="h-8 flex-1 font-mono"
			/>
			{#if scope.length > 0 && !readonly}
				<InsertRefButton
					{scope}
					placeholder="Insert ref…"
					triggerClass="h-8 w-auto shrink-0 px-2"
					oninsert={(snippet) => appendRefToValue(i, snippet)}
				/>
			{/if}
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
			class="flex w-full items-center justify-center gap-1 rounded border border-dashed border-border py-1 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
			onclick={addRow}
		>
			<Plus class="size-3" />
			Add Entry
		</button>
	{/if}

	{#if rows.length === 0 && readonly}
		<p class="text-sm italic text-muted-foreground">No entries</p>
	{/if}
</div>
