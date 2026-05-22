<script lang="ts">
	import KeyValueEditor from '../../shared/KeyValueEditor.svelte';
	import StringListEditor from '../../shared/StringListEditor.svelte';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type * as Y from 'yjs';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { Input } from '$lib/components/ui/input';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { FormField } from '$lib/components/ui/form-field';

	type Props = {
		config: Record<string, unknown>;
		entrypoint?: string;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
		onentrypointchange?: (entrypoint: string) => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
		templateId?: string;
		/** Read-only listing of upstream fields this step can read at runtime.
		 *  Actual code-editor insertion happens in the IDE side. */
		scope?: ScopeEntry[];
	};

	let {
		config,
		entrypoint = 'main.py',
		readonly = false,
		onchange,
		onentrypointchange,
		binding,
		nodeId,
		templateId,
		scope = []
	}: Props = $props();

	function fileHref(filename: string): string | null {
		if (!templateId || !nodeId) return null;
		const params = new URLSearchParams({
			node: nodeId,
			file: `${nodeId}:${filename}`
		});
		return `/templates/${templateId}/ide?${params.toString()}`;
	}

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

{#if scope.length > 0}
	<div class="space-y-1" data-testid="python-inputs-in-scope">
		<span class="text-sm font-medium text-muted-foreground">Inputs in scope</span>
		<ul class="rounded-md border border-border/60 bg-muted/20 p-2 space-y-0.5">
			{#each scope as e (e.qualified)}
				<li class="flex items-baseline justify-between gap-2 text-sm">
					<code class="font-mono text-foreground">{e.qualified}</code>
					<span class="shrink-0 text-sm text-muted-foreground">{e.kind}</span>
				</li>
			{/each}
		</ul>
		<p class="text-sm text-muted-foreground">
			Readable at runtime as <code class="font-mono">token["…"]</code> or via the typed
			<code class="font-mono">load_input()</code> helper. Open the IDE to insert into code.
		</p>
	</div>
{/if}

<div class="space-y-1.5">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Files</span>
		{#if !readonly && binding && nodeId}
			<button
				type="button"
				class="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
				onclick={handleCreateFile}
			>
				<Plus class="size-3" />
				New file
			</button>
		{/if}
	</div>

	{#if filenames.length === 0}
		<p class="text-sm italic text-muted-foreground">
			No files yet. Add one (or open the IDE editor) — the entrypoint must exist.
		</p>
	{:else}
		<div class="flex flex-col gap-0.5">
			{#each filenames as filename (filename)}
				{@const href = fileHref(filename)}
				<div
					class="group flex items-center gap-1 rounded border px-2 py-1 text-sm transition-colors {filename ===
					entrypoint
						? 'border-primary bg-primary/5 text-foreground'
						: 'border-border text-muted-foreground hover:bg-accent/30'}"
				>
					{#if href}
						<a
							{href}
							class="flex-1 truncate font-mono hover:text-foreground hover:underline"
							title="Open {filename} in IDE"
							data-testid="file-link-{filename}"
						>{filename}</a>
					{:else}
						<span class="flex-1 truncate font-mono">{filename}</span>
					{/if}
					{#if filename === entrypoint}
						<span class="rounded bg-primary/10 px-1 py-px text-sm uppercase tracking-wider text-primary">
							entry
						</span>
					{:else if !readonly}
						<button
							type="button"
							class="rounded px-1 py-px text-sm uppercase tracking-wider text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
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

<FormField
	label="Entrypoint"
	for="entrypoint"
	error={filenames.length > 0 && !filenames.includes(entrypoint)
		? `Entrypoint ${entrypoint} is not in the file list — publish will fail.`
		: undefined}
>
	<Input
		id="entrypoint"
		type="text"
		value={entrypoint}
		disabled={readonly}
		oninput={(e) => handleEntrypoint((e.currentTarget as HTMLInputElement).value)}
		placeholder="main.py"
		class="font-mono"
	/>
</FormField>

<FormField label="Python Binary" for="python-bin">
	<Input
		id="python-bin"
		type="text"
		value={(config.python as string) ?? 'python3'}
		placeholder="python3"
		disabled={readonly}
		oninput={(e) => onchange({ ...config, python: (e.currentTarget as HTMLInputElement).value })}
		class="font-mono"
	/>
</FormField>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Pip Requirements</span>
	<StringListEditor
		items={(config.requirements as string[]) ?? []}
		{readonly}
		placeholder="e.g. numpy==1.24.0"
		onchange={(requirements) => onchange({ ...config, requirements })}
	/>
</div>

<div class="flex flex-wrap items-center gap-3">
	<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
		<Checkbox
			checked={(config.virtualenv as boolean) ?? false}
			disabled={readonly}
			onCheckedChange={(checked) => onchange({ ...config, virtualenv: checked === true })}
		/>
		Virtualenv
	</label>
	<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
		<Checkbox
			checked={(config.sdk as boolean) ?? true}
			disabled={readonly}
			onCheckedChange={(checked) => onchange({ ...config, sdk: checked === true })}
		/>
		SDK
	</label>
	<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
		<Checkbox
			checked={(config.inherit_env as boolean) ?? true}
			disabled={readonly}
			onCheckedChange={(checked) => onchange({ ...config, inherit_env: checked === true })}
		/>
		Inherit Env
	</label>
</div>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Environment Variables</span>
	<KeyValueEditor
		entries={(config.env as Record<string, unknown>) ?? {}}
		{readonly}
		keyPlaceholder="VAR_NAME"
		valuePlaceholder="value"
		onchange={(env) => onchange({ ...config, env })}
	/>
</div>
