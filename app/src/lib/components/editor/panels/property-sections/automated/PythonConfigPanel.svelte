<script lang="ts">
	import CodeEditor from '../../shared/CodeEditor.svelte';
	import CollabCodeEditor from '../../shared/CollabCodeEditor.svelte';
	import KeyValueEditor from '../../shared/KeyValueEditor.svelte';
	import StringListEditor from '../../shared/StringListEditor.svelte';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type * as Y from 'yjs';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
		onexpand?: () => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
	};

	let { config, readonly = false, onchange, onexpand, binding, nodeId }: Props = $props();

	type ScriptMode = 'file' | 'inline' | 'node_file';

	const scriptMode = $derived<ScriptMode>(
		config.nodeFile ? 'node_file' : typeof config.scriptContent === 'string' ? 'inline' : 'file'
	);

	// Get files from binding when in node_file mode
	const nodeFiles = $derived(
		binding && nodeId ? binding.getNodeFiles(nodeId) : new Map<string, Y.Text>()
	);

	let selectedFileName = $state<string | null>(null);

	const selectedYText = $derived(
		selectedFileName ? nodeFiles.get(selectedFileName) ?? null : null
	);

	function setMode(mode: ScriptMode) {
		if (mode === 'file') {
			const { scriptContent: _1, nodeFile: _2, ...rest } = config;
			onchange({ ...rest, script: config.script ?? '' });
		} else if (mode === 'inline') {
			const { script: _1, nodeFile: _2, ...rest } = config;
			onchange({ ...rest, scriptContent: config.scriptContent ?? '' });
			onexpand?.();
		} else {
			const { script: _1, scriptContent: _2, ...rest } = config;
			onchange({ ...rest, nodeFile: selectedFileName ?? 'main.py' });
			onexpand?.();
		}
	}

	function handleCreateFile() {
		if (!binding || !nodeId) return;
		const filename = 'main.py';
		binding.createFile(nodeId, filename, '# New Python script\n');
		selectedFileName = filename;
		onchange({ ...config, nodeFile: filename });
	}

	function handleDeleteFile(filename: string) {
		if (!binding || !nodeId) return;
		binding.deleteFile(nodeId, filename);
		if (selectedFileName === filename) {
			selectedFileName = null;
		}
	}

	function handleSelectFile(filename: string) {
		selectedFileName = filename;
		onchange({ ...config, nodeFile: filename });
	}
</script>

{#if !readonly}
	<div class="space-y-1.5">
		<span class="text-xs font-medium text-muted-foreground">Script Source</span>
		<div class="flex gap-1">
			<button
				type="button"
				class="flex-1 rounded-md border px-2 py-1 text-[10px] font-medium transition-colors {scriptMode === 'file'
					? 'border-primary bg-primary/10 text-primary'
					: 'border-border text-muted-foreground hover:bg-accent'}"
				onclick={() => setMode('file')}
			>
				File Reference
			</button>
			<button
				type="button"
				class="flex-1 rounded-md border px-2 py-1 text-[10px] font-medium transition-colors {scriptMode === 'inline'
					? 'border-primary bg-primary/10 text-primary'
					: 'border-border text-muted-foreground hover:bg-accent'}"
				onclick={() => setMode('inline')}
			>
				Inline Script
			</button>
			{#if binding && nodeId}
				<button
					type="button"
					class="flex-1 rounded-md border px-2 py-1 text-[10px] font-medium transition-colors {scriptMode === 'node_file'
						? 'border-primary bg-primary/10 text-primary'
						: 'border-border text-muted-foreground hover:bg-accent'}"
					onclick={() => setMode('node_file')}
				>
					Node File
				</button>
			{/if}
		</div>
	</div>
{/if}

{#if scriptMode === 'file'}
	<div class="space-y-1.5">
		<label for="script-file" class="text-xs font-medium text-muted-foreground">Script File</label>
		<input
			id="script-file"
			type="text"
			value={(config.script as string) ?? ''}
			placeholder="e.g. extract_invoice.py"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, script: (e.currentTarget as HTMLInputElement).value })}
			class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
{:else if scriptMode === 'inline'}
	<div class="space-y-1.5">
		<span class="text-xs font-medium text-muted-foreground">Python Script</span>
		<CodeEditor
			value={(config.scriptContent as string) ?? ''}
			language="python"
			{readonly}
			minHeight="150px"
			maxHeight="400px"
			onchange={(val) => onchange({ ...config, scriptContent: val })}
		/>
	</div>
{:else if scriptMode === 'node_file' && binding && nodeId}
	<div class="space-y-1.5">
		<div class="flex items-center justify-between">
			<span class="text-xs font-medium text-muted-foreground">Node Files</span>
			{#if !readonly}
				<button
					type="button"
					class="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[10px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
					onclick={handleCreateFile}
				>
					<Plus class="size-3" />
					Create File
				</button>
			{/if}
		</div>

		{#if nodeFiles.size === 0}
			<p class="text-xs text-muted-foreground italic">No files yet. Create one to start editing.</p>
		{:else}
			<div class="flex flex-col gap-1">
				{#each [...nodeFiles.keys()] as filename}
					<div
						class="flex items-center justify-between rounded-md border px-2 py-1 text-xs transition-colors {selectedFileName === filename
							? 'border-primary bg-primary/5 text-foreground'
							: 'border-border text-muted-foreground hover:bg-accent cursor-pointer'}"
					>
						<button
							type="button"
							class="flex-1 text-left font-mono"
							onclick={() => handleSelectFile(filename)}
						>
							{filename}
						</button>
						{#if !readonly}
							<button
								type="button"
								class="ml-1 rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
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

		{#if selectedYText}
			<CollabCodeEditor
				ytext={selectedYText}
				language="python"
				{readonly}
				awareness={binding ? undefined : undefined}
				minHeight="150px"
				maxHeight="400px"
			/>
		{/if}
	</div>
{/if}

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
	<label for="timeout" class="text-xs font-medium text-muted-foreground">Timeout (seconds)</label>
	<input
		id="timeout"
		type="number"
		min={1}
		value={(config.timeout_seconds as number) ?? 30}
		disabled={readonly}
		oninput={(e) =>
			onchange({
				...config,
				timeout_seconds: parseInt((e.currentTarget as HTMLInputElement).value) || 30
			})}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
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

<div class="flex items-center gap-2">
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
