<script lang="ts">
	import CodeEditor from '../../shared/CodeEditor.svelte';
	import KeyValueEditor from '../../shared/KeyValueEditor.svelte';
	import StringListEditor from '../../shared/StringListEditor.svelte';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
	};

	let { config, readonly = false, onchange }: Props = $props();

	const scriptMode = $derived(
		typeof config.scriptContent === 'string' ? 'inline' : 'file'
	);

	function setMode(mode: 'file' | 'inline') {
		if (mode === 'file') {
			const { scriptContent: _, ...rest } = config;
			onchange({ ...rest, script: config.script ?? '' });
		} else {
			const { script: _, ...rest } = config;
			onchange({ ...rest, scriptContent: config.scriptContent ?? '' });
		}
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
{:else}
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
