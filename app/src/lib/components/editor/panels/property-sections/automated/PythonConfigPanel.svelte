<script lang="ts">
	import KeyValueEditor from '../../shared/KeyValueEditor.svelte';
	import StringListEditor from '../../shared/StringListEditor.svelte';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type * as Y from 'yjs';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';

	type Props = {
		config: Record<string, unknown>;
		entrypoint?: string;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
		onentrypointchange?: (entrypoint: string) => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
	};

	let {
		config,
		entrypoint = 'main.py',
		readonly = false,
		onchange,
		onentrypointchange,
		binding,
		nodeId
	}: Props = $props();

	// Files for this node (collaborative). Empty Map when binding/nodeId aren't
	// provided (e.g. in test harnesses); the panel still renders the rest of the
	// config so other knobs work.
	const nodeFiles: Map<string, Y.Text> = $derived(
		binding && nodeId ? binding.getNodeFiles(nodeId) : new Map<string, Y.Text>()
	);

	const filenames = $derived([...nodeFiles.keys()].sort());

	function handleEntrypoint(name: string) {
		onentrypointchange?.(name);
	}

	function handleCreateFile() {
		if (!binding || !nodeId) return;
		const name = prompt('File name:', 'helper.py');
		if (!name) return;
		binding.createFile(nodeId, name, '');
		// Don't auto-switch entrypoint — the user picks via the dropdown.
	}

	function handleDeleteFile(filename: string) {
		if (!binding || !nodeId) return;
		if (filename === entrypoint) {
			alert(`Cannot delete the entrypoint (${filename}). Switch entrypoint first.`);
			return;
		}
		if (!confirm(`Delete ${filename}?`)) return;
		binding.deleteFile(nodeId, filename);
	}
</script>

<div class="space-y-1.5">
	<div class="flex items-center justify-between">
		<span class="text-xs font-medium text-muted-foreground">Files</span>
		{#if !readonly && binding && nodeId}
			<button
				type="button"
				class="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[10px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
				onclick={handleCreateFile}
			>
				<Plus class="size-3" />
				New file
			</button>
		{/if}
	</div>

	{#if filenames.length === 0}
		<p class="text-[11px] italic text-muted-foreground">
			No files yet. Add one (or open the IDE editor) — the entrypoint must exist.
		</p>
	{:else}
		<div class="flex flex-col gap-0.5">
			{#each filenames as filename (filename)}
				<div
					class="group flex items-center gap-1 rounded border px-2 py-1 text-xs transition-colors {filename ===
					entrypoint
						? 'border-primary bg-primary/5 text-foreground'
						: 'border-border text-muted-foreground hover:bg-accent/30'}"
				>
					<span class="flex-1 truncate font-mono">{filename}</span>
					{#if filename === entrypoint}
						<span class="rounded bg-primary/10 px-1 py-px text-[9px] uppercase tracking-wider text-primary">
							entry
						</span>
					{:else if !readonly}
						<button
							type="button"
							class="rounded px-1 py-px text-[9px] uppercase tracking-wider text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
							onclick={() => handleEntrypoint(filename)}
						>
							set entry
						</button>
					{/if}
					{#if !readonly && binding && nodeId}
						<button
							type="button"
							class="rounded p-0.5 text-muted-foreground opacity-0 transition-all group-hover:opacity-100 hover:text-destructive"
							onclick={() => handleDeleteFile(filename)}
							title="Delete file"
						>
							<Trash2 class="size-3" />
						</button>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</div>

<div class="space-y-1.5">
	<label for="entrypoint" class="text-xs font-medium text-muted-foreground">Entrypoint</label>
	<input
		id="entrypoint"
		type="text"
		value={entrypoint}
		disabled={readonly}
		oninput={(e) => handleEntrypoint((e.currentTarget as HTMLInputElement).value)}
		placeholder="main.py"
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
	{#if filenames.length > 0 && !filenames.includes(entrypoint)}
		<p class="text-[11px] text-amber-700">
			Entrypoint <code class="font-mono">{entrypoint}</code> is not in the file list — publish
			will fail.
		</p>
	{/if}
</div>

<div class="space-y-1.5">
	<label for="python-bin" class="text-xs font-medium text-muted-foreground">Python Binary</label>
	<input
		id="python-bin"
		type="text"
		value={(config.python as string) ?? 'python3'}
		placeholder="python3"
		disabled={readonly}
		oninput={(e) => onchange({ ...config, python: (e.currentTarget as HTMLInputElement).value })}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Pip Requirements</span>
	<StringListEditor
		items={(config.requirements as string[]) ?? []}
		{readonly}
		placeholder="e.g. numpy==1.24.0"
		onchange={(requirements) => onchange({ ...config, requirements })}
	/>
</div>

<div class="flex flex-wrap items-center gap-3">
	<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
		<input
			type="checkbox"
			checked={(config.virtualenv as boolean) ?? false}
			disabled={readonly}
			onchange={(e) =>
				onchange({ ...config, virtualenv: (e.currentTarget as HTMLInputElement).checked })}
			class="size-3.5 disabled:cursor-default disabled:opacity-70"
		/>
		Virtualenv
	</label>
	<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
		<input
			type="checkbox"
			checked={(config.sdk as boolean) ?? true}
			disabled={readonly}
			onchange={(e) =>
				onchange({ ...config, sdk: (e.currentTarget as HTMLInputElement).checked })}
			class="size-3.5 disabled:cursor-default disabled:opacity-70"
		/>
		SDK
	</label>
	<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
		<input
			type="checkbox"
			checked={(config.inherit_env as boolean) ?? true}
			disabled={readonly}
			onchange={(e) =>
				onchange({ ...config, inherit_env: (e.currentTarget as HTMLInputElement).checked })}
			class="size-3.5 disabled:cursor-default disabled:opacity-70"
		/>
		Inherit Env
	</label>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Environment Variables</span>
	<KeyValueEditor
		entries={(config.env as Record<string, unknown>) ?? {}}
		{readonly}
		keyPlaceholder="VAR_NAME"
		valuePlaceholder="value"
		onchange={(env) => onchange({ ...config, env })}
	/>
</div>
