<script lang="ts">
	import type { DownloadBlock } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Plus from '@lucide/svelte/icons/plus';
	import Download from '@lucide/svelte/icons/download';
	import { Input } from '$lib/components/ui/input';
	import InterpolationHint from './InterpolationHint.svelte';
	import InsertRefButton from '../InsertRefButton.svelte';

	type DownloadItem = DownloadBlock['downloads'][number];

	type Props = {
		downloads: DownloadItem[];
		readonly?: boolean;
		scope?: ScopeEntry[];
		onchange: (downloads: DownloadItem[]) => void;
		onremove: () => void;
	};

	let { downloads, readonly = false, scope = [], onchange, onremove }: Props = $props();

	function updateItem(index: number, patch: Partial<DownloadItem>) {
		const next = downloads.map((d, i) => (i === index ? { ...d, ...patch } : d));
		onchange(next);
	}

	function appendField(
		index: number,
		key: 'url' | 'filename' | 'description',
		snippet: string
	) {
		const curr = downloads[index]?.[key] ?? '';
		const next = curr ? `${curr} ${snippet}` : snippet;
		updateItem(index, { [key]: next } as Partial<DownloadItem>);
	}

	function addItem() {
		onchange([...downloads, { url: '', filename: '' }]);
	}

	function removeItem(index: number) {
		onchange(downloads.filter((_, i) => i !== index));
	}

	function trimOrUndefined(v: string): string | undefined {
		return v.trim() ? v : undefined;
	}
</script>

<!-- ui-allow: block-type accent — no theme token for download/indigo identity -->
<div class="rounded-md border border-border/50 border-l-2 border-l-indigo-400 bg-background p-3">
	<div class="mb-2 flex items-center justify-between">
		<!-- ui-allow: block-type badge color — no theme token for download/indigo identity -->
		<span
			class="rounded bg-indigo-100 px-2 py-0.5 text-sm font-medium text-indigo-700 dark:bg-indigo-900/30 dark:text-indigo-300"
		>
			Download
		</span>
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

	<div class="space-y-3">
		{#if downloads.length === 0}
			<div
				class="flex items-center gap-2 rounded-md border border-dashed border-border p-4 text-sm text-muted-foreground"
			>
				<Download class="size-4 shrink-0" />
				<span>No downloads yet. Add one below.</span>
			</div>
		{/if}

		{#each downloads as item, idx (idx)}
			<div class="space-y-1.5 rounded-md border border-border bg-muted/20 p-2">
				<div class="flex items-center justify-between">
					<span class="text-sm font-medium text-muted-foreground">Download {idx + 1}</span>
					{#if !readonly}
						<button
							type="button"
							class="rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
							onclick={() => removeItem(idx)}
						>
							<Trash2 class="size-3.5" />
						</button>
					{/if}
				</div>
				<div class="space-y-1">
					<Input
						type="text"
						value={item.url}
						placeholder={'URL — e.g. {{ invoice_file.url }}'}
						disabled={readonly}
						oninput={(e) => updateItem(idx, { url: (e.currentTarget as HTMLInputElement).value })}
						class="font-mono text-sm"
					/>
					{#if scope.length > 0}
						<InsertRefButton
							{scope}
							disabled={readonly}
							oninsert={(s) => appendField(idx, 'url', s)}
						/>
					{/if}
				</div>
				<div class="grid grid-cols-2 gap-2">
					<div class="space-y-1">
						<Input
							type="text"
							value={item.filename}
							placeholder={'Filename — e.g. {{ invoice_file.filename }}'}
							disabled={readonly}
							oninput={(e) =>
								updateItem(idx, { filename: (e.currentTarget as HTMLInputElement).value })}
							class="font-mono text-sm"
						/>
						{#if scope.length > 0}
							<InsertRefButton
								{scope}
								disabled={readonly}
								oninsert={(s) => appendField(idx, 'filename', s)}
							/>
						{/if}
					</div>
					<Input
						type="text"
						value={item.mime_type ?? ''}
						placeholder={'MIME type (optional)'}
						disabled={readonly}
						oninput={(e) =>
							updateItem(idx, {
								mime_type: trimOrUndefined((e.currentTarget as HTMLInputElement).value)
							})}
						class="font-mono text-sm"
					/>
				</div>
				<div class="space-y-1">
					<Input
						type="text"
						value={item.description ?? ''}
						placeholder="Description (optional)"
						disabled={readonly}
						oninput={(e) =>
							updateItem(idx, {
								description: trimOrUndefined((e.currentTarget as HTMLInputElement).value)
							})}
					/>
					{#if scope.length > 0}
						<InsertRefButton
							{scope}
							disabled={readonly}
							oninsert={(s) => appendField(idx, 'description', s)}
						/>
					{/if}
				</div>
			</div>
		{/each}

		{#if scope.length === 0}
			<InterpolationHint example="invoice_file.url" />
		{/if}

		{#if !readonly}
			<button
				type="button"
				class="flex w-full items-center justify-center gap-1.5 rounded-md border border-dashed border-border py-1.5 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
				onclick={addItem}
			>
				<Plus class="size-3.5" />
				Add download
			</button>
		{/if}
	</div>
</div>
