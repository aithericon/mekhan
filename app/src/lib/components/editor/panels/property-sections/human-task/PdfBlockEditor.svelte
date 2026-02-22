<script lang="ts">
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import FileText from '@lucide/svelte/icons/file-text';
	import { Select, SelectTrigger, SelectContent, SelectItem } from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';

	type Props = {
		filename: string;
		caption?: string;
		height?: string;
		binding?: YjsGraphBinding;
		nodeId?: string;
		readonly?: boolean;
		onchange: (filename: string, caption?: string, height?: string) => void;
		onremove: () => void;
	};

	let { filename, caption, height, binding, nodeId, readonly = false, onchange, onremove }: Props = $props();

	const pdfFiles = $derived.by(() => {
		if (!binding || !nodeId) return [];
		const files = binding.getNodeFiles(nodeId);
		return [...files.keys()].filter((name) => name.toLowerCase().endsWith('.pdf'));
	});

	const previewUrl = $derived(
		filename ? `/api/files/${filename}` : ''
	);
</script>

<div class="rounded-md border border-border/50 border-l-2 border-l-rose-400 bg-background p-3">
	<div class="mb-2 flex items-center justify-between">
		<span class="rounded bg-rose-100 px-2 py-0.5 text-xs font-medium text-rose-700 dark:bg-rose-900/30 dark:text-rose-300">
			PDF
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

	{#if pdfFiles.length === 0}
		<div class="flex items-center gap-2 rounded-md border border-dashed border-border p-4 text-sm text-muted-foreground">
			<FileText class="size-4 shrink-0" />
			<span>No PDF files uploaded yet. Use the upload button in the file tree.</span>
		</div>
	{:else}
		<div class="space-y-3">
			<Select.Root
				type="single"
				value={filename}
				onValueChange={(v) => { if (v) onchange(v, caption, height); }}
				disabled={readonly}
			>
				<SelectTrigger disabled={readonly} class="h-9 px-2 text-sm">
					{#if filename}
						<span class="flex items-center gap-1.5">
							<FileText class="size-3.5 shrink-0 text-muted-foreground" />
							<span class="truncate font-mono">{filename}</span>
						</span>
					{:else}
						<span class="text-muted-foreground">Select a PDF...</span>
					{/if}
				</SelectTrigger>
				<SelectContent>
					{#each pdfFiles as name (name)}
						<SelectItem value={name} label={name} />
					{/each}
				</SelectContent>
			</Select.Root>

			{#if filename}
				<iframe
					src={previewUrl}
					title={caption || filename}
					class="w-full rounded-md border border-border"
					style="height: {height || '400px'}"
				></iframe>
			{/if}

			<div class="grid grid-cols-2 gap-2">
				<Input
					type="text"
					value={caption ?? ''}
					placeholder="Caption (optional)"
					disabled={readonly}
					oninput={(e) => onchange(filename, (e.currentTarget as HTMLInputElement).value || undefined, height)}
				/>
				<Input
					type="text"
					value={height ?? '400px'}
					placeholder="Height (e.g. 400px)"
					disabled={readonly}
					oninput={(e) => onchange(filename, caption, (e.currentTarget as HTMLInputElement).value || undefined)}
				/>
			</div>
		</div>
	{/if}
</div>
