<script lang="ts">
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import File from '@lucide/svelte/icons/file';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';

	type FileSelection = { nodeId: string; filename: string };

	type Props = {
		binding: YjsGraphBinding;
		selectedFile?: FileSelection;
		selectedNodeId?: string | null;
		onSelectFile: (nodeId: string, filename: string) => void;
		onSelectNode?: (nodeId: string) => void;
		onCreateFile: (nodeId: string) => void;
		onDeleteFile: (nodeId: string, filename: string) => void;
		onRenameFile: (nodeId: string, oldName: string, newName: string) => void;
	};

	let { binding, selectedFile, selectedNodeId, onSelectFile, onSelectNode, onCreateFile, onDeleteFile, onRenameFile }: Props = $props();

	const expandedNodes = new SvelteSet<string>();

	function toggleNode(nodeId: string) {
		if (expandedNodes.has(nodeId)) {
			expandedNodes.delete(nodeId);
		} else {
			expandedNodes.add(nodeId);
		}
	}

	// Auto-expand the tree node containing the selected file (e.g. after URL restore)
	$effect(() => {
		if (selectedFile) {
			expandedNodes.add(selectedFile.nodeId);
		}
	});

	function isSelected(nodeId: string, filename: string): boolean {
		return selectedFile?.nodeId === nodeId && selectedFile?.filename === filename;
	}
</script>

<div class="flex h-full flex-col overflow-y-auto border-r border-border bg-card">
	<div class="border-b border-border px-3 py-2">
		<span class="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Files</span>
	</div>

	<div class="flex-1 overflow-y-auto py-1">
		{#each binding.graph.nodes as node (node.id)}
			{@const files = binding.getNodeFiles(node.id)}
			{@const isExpanded = expandedNodes.has(node.id)}
			<div>
				<div
					class="flex w-full items-center gap-1 px-2 py-1 text-xs transition-colors hover:bg-accent {selectedNodeId === node.id && !selectedFile ? 'bg-accent text-foreground' : 'text-foreground'}"
				>
					<button
						type="button"
						class="flex flex-1 items-center gap-1 truncate text-left"
						onclick={() => { toggleNode(node.id); onSelectNode?.(node.id); }}
					>
						{#if isExpanded}
							<ChevronDown class="size-3 shrink-0 text-muted-foreground" />
						{:else}
							<ChevronRight class="size-3 shrink-0 text-muted-foreground" />
						{/if}
						<span class="truncate font-medium">{node.data.label}</span>
					</button>
					<button
						type="button"
						class="rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
						onclick={() => onCreateFile(node.id)}
						title="Create file"
					>
						<Plus class="size-3" />
					</button>
				</div>

				{#if isExpanded}
					{#if files.size === 0}
						<div class="py-1 pl-7 text-[10px] italic text-muted-foreground">No files</div>
					{:else}
						{#each [...files.keys()] as filename (filename)}
							<div
								class="group flex items-center gap-1 py-0.5 pl-6 pr-2 text-xs transition-colors {isSelected(node.id, filename)
									? 'bg-accent text-foreground'
									: 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'}"
							>
								<button
									type="button"
									class="flex flex-1 items-center gap-1.5 truncate text-left"
									onclick={() => onSelectFile(node.id, filename)}
								>
									<File class="size-3 shrink-0" />
									<span class="truncate font-mono">{filename}</span>
								</button>
								<button
									type="button"
									class="rounded p-0.5 text-muted-foreground opacity-0 transition-all group-hover:opacity-100 hover:text-destructive"
									onclick={() => onDeleteFile(node.id, filename)}
									title="Delete file"
								>
									<Trash2 class="size-3" />
								</button>
							</div>
						{/each}
					{/if}
				{/if}
			</div>
		{/each}
	</div>
</div>
