<script lang="ts">
	import KeyValueEditor from '../../shared/KeyValueEditor.svelte';
	import StringListEditor from '../../shared/StringListEditor.svelte';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
	};

	let { config, readonly = false, onchange }: Props = $props();
</script>

<div class="space-y-1.5">
	<label for="process-command" class="text-xs font-medium text-muted-foreground">Command</label>
	<input
		id="process-command"
		type="text"
		value={(config.command as string) ?? ''}
		placeholder="e.g. python3"
		disabled={readonly}
		oninput={(e) => onchange({ ...config, command: (e.currentTarget as HTMLInputElement).value })}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Arguments</span>
	<StringListEditor
		items={(config.args as string[]) ?? []}
		{readonly}
		placeholder="Argument"
		onchange={(args) => onchange({ ...config, args })}
	/>
</div>

<div class="space-y-1.5">
	<label for="process-workdir" class="text-xs font-medium text-muted-foreground"
		>Working Directory (optional)</label
	>
	<input
		id="process-workdir"
		type="text"
		value={(config.working_dir as string) ?? ''}
		placeholder="/data"
		disabled={readonly}
		oninput={(e) =>
			onchange({ ...config, working_dir: (e.currentTarget as HTMLInputElement).value || undefined })}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
</div>

<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
	<input
		type="checkbox"
		checked={(config.inherit_env as boolean) ?? true}
		disabled={readonly}
		onchange={(e) =>
			onchange({ ...config, inherit_env: (e.currentTarget as HTMLInputElement).checked })}
		class="size-3.5 disabled:cursor-default disabled:opacity-70"
	/>
	Inherit environment
</label>

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
