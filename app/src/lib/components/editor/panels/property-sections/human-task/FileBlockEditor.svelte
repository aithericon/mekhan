<script lang="ts">
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import FileIcon from '@lucide/svelte/icons/file';
	import FileText from '@lucide/svelte/icons/file-text';
	import { Select, SelectTrigger, SelectContent, SelectItem } from '$lib/components/ui/select';

	type Props = {
		filename: string;
		binding?: YjsGraphBinding;
		nodeId?: string;
		readonly?: boolean;
		onchange: (filename: string) => void;
		onremove: () => void;
	};

	let { filename, binding, nodeId, readonly = false, onchange, onremove }: Props = $props();

	const IMAGE_EXTENSIONS = ['.png', '.jpg', '.jpeg', '.gif', '.webp', '.svg'];

	function isImageFile(name: string): boolean {
		return IMAGE_EXTENSIONS.some((ext) => name.toLowerCase().endsWith(ext));
	}

	const allFiles = $derived.by(() => {
		if (!binding || !nodeId) return [];
		const files = binding.getNodeFiles(nodeId);
		return [...files.keys()].filter((name) => !isImageFile(name));
	});

	function getExtension(name: string): string {
		const dot = name.lastIndexOf('.');
		return dot >= 0 ? name.slice(dot + 1).toUpperCase() : 'FILE';
	}
</script>

<div class="rounded-md border border-border/50 border-l-2 border-l-sky-400 bg-background p-3">
	<div class="mb-2 flex items-center justify-between">
		<span class="rounded bg-sky-100 px-2 py-0.5 text-xs font-medium text-sky-700 dark:bg-sky-900/30 dark:text-sky-300">
			File
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

	{#if allFiles.length === 0}
		<div class="flex items-center gap-2 rounded-md border border-dashed border-border p-4 text-sm text-muted-foreground">
			<FileIcon class="size-4 shrink-0" />
			<span>No files uploaded yet. Use the upload button in the file tree.</span>
		</div>
	{:else}
		<div class="space-y-3">
			<Select.Root
				type="single"
				value={filename}
				onValueChange={(v) => { if (v) onchange(v); }}
				disabled={readonly}
			>
				<SelectTrigger disabled={readonly} class="h-9 px-2 text-sm">
					{#if filename}
						<span class="flex items-center gap-1.5">
							<FileText class="size-3.5 shrink-0 text-muted-foreground" />
							<span class="truncate font-mono">{filename}</span>
						</span>
					{:else}
						<span class="text-muted-foreground">Select a file...</span>
					{/if}
				</SelectTrigger>
				<SelectContent>
					{#each allFiles as name (name)}
						<SelectItem value={name} label={name} />
					{/each}
				</SelectContent>
			</Select.Root>

			{#if filename}
				<div class="flex items-center gap-2 rounded-md border border-border bg-muted/30 px-3 py-2">
					<FileText class="size-5 shrink-0 text-muted-foreground" />
					<span class="flex-1 truncate font-mono text-sm">{filename}</span>
					<span class="rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
						{getExtension(filename)}
					</span>
				</div>
			{/if}
		</div>
	{/if}
</div>
