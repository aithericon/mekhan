<script lang="ts">
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import FileText from '@lucide/svelte/icons/file-text';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import InterpolationHint from './InterpolationHint.svelte';

	type Props = {
		filename?: string;
		caption?: string;
		height?: string;
		url?: string;
		binding?: YjsGraphBinding;
		nodeId?: string;
		readonly?: boolean;
		onchange: (filename: string, caption?: string, height?: string, url?: string) => void;
		onremove: () => void;
	};

	let {
		filename: filenameProp,
		caption,
		height,
		url,
		binding,
		nodeId,
		readonly = false,
		onchange,
		onremove
	}: Props = $props();

	const filename = $derived(filenameProp ?? '');

	const pdfFiles = $derived.by(() => {
		if (!binding || !nodeId) return [];
		const files = binding.getNodeFiles(nodeId);
		return [...files.keys()].filter((name) => name.toLowerCase().endsWith('.pdf'));
	});

	const previewUrl = $derived(filename ? `/api/files/${filename}` : '');

	function setUrl(value: string) {
		onchange(filename, caption, height, value || undefined);
	}
</script>

<!-- ui-allow: block-type accent — no theme token for pdf/rose identity -->
<div class="rounded-md border border-border/50 border-l-2 border-l-rose-400 bg-background p-3">
	<div class="mb-2 flex items-center justify-between">
		<!-- ui-allow: block-type badge color — no theme token for pdf/rose identity -->
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

	<!-- Dynamic source (URL / interpolated) -->
	<div class="mb-3 space-y-1">
		<Input
			type="text"
			value={url ?? ''}
			placeholder={'Dynamic source URL — e.g. {{ invoice_file.url }}'}
			disabled={readonly}
			oninput={(e) => setUrl((e.currentTarget as HTMLInputElement).value)}
			class="font-mono text-xs"
		/>
		<InterpolationHint example="invoice_file.url" />
		{#if url}
			<p class="text-[11px] text-muted-foreground">
				A dynamic source is set — it takes precedence over an uploaded file when the task renders.
			</p>
		{/if}
	</div>

	{#if pdfFiles.length === 0}
		<div class="flex items-center gap-2 rounded-md border border-dashed border-border p-4 text-sm text-muted-foreground">
			<FileText class="size-4 shrink-0" />
			<span>No PDF files uploaded yet. Add a dynamic source above, or upload via the file tree.</span>
		</div>
	{:else}
		<div class="space-y-3">
			<Select.Root
				type="single"
				value={filename}
				onValueChange={(v) => { if (v) onchange(v, caption, height, url); }}
				disabled={readonly}
			>
				<Select.Trigger disabled={readonly} class="h-9 px-2 text-sm">
					{#if filename}
						<span class="flex items-center gap-1.5">
							<FileText class="size-3.5 shrink-0 text-muted-foreground" />
							<span class="truncate font-mono">{filename}</span>
						</span>
					{:else}
						<span class="text-muted-foreground">Select a PDF...</span>
					{/if}
				</Select.Trigger>
				<Select.Content>
					{#each pdfFiles as name (name)}
						<Select.Item value={name} label={name} />
					{/each}
				</Select.Content>
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
					oninput={(e) => onchange(filename, (e.currentTarget as HTMLInputElement).value || undefined, height, url)}
				/>
				<Input
					type="text"
					value={height ?? '400px'}
					placeholder="Height (e.g. 400px)"
					disabled={readonly}
					oninput={(e) => onchange(filename, caption, (e.currentTarget as HTMLInputElement).value || undefined, url)}
				/>
			</div>
		</div>
	{/if}
</div>
