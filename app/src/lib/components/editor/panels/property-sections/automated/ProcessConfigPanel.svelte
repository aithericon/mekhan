<script lang="ts">
	import KeyValueEditor from '../../shared/KeyValueEditor.svelte';
	import StringListEditor from '../../shared/StringListEditor.svelte';
	import { Input } from '$lib/components/ui/input';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { FormField } from '$lib/components/ui/form-field';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
	};

	let { config, readonly = false, onchange }: Props = $props();
</script>

<FormField label="Command" for="process-command">
	<Input
		id="process-command"
		type="text"
		value={(config.command as string) ?? ''}
		placeholder="e.g. python3"
		disabled={readonly}
		oninput={(e) => onchange({ ...config, command: (e.currentTarget as HTMLInputElement).value })}
		class="font-mono"
	/>
</FormField>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Arguments</span>
	<StringListEditor
		items={(config.args as string[]) ?? []}
		{readonly}
		placeholder="Argument"
		onchange={(args) => onchange({ ...config, args })}
	/>
</div>

<FormField label="Working Directory (optional)" for="process-workdir">
	<Input
		id="process-workdir"
		type="text"
		value={(config.working_dir as string) ?? ''}
		placeholder="/data"
		disabled={readonly}
		oninput={(e) =>
			onchange({ ...config, working_dir: (e.currentTarget as HTMLInputElement).value || undefined })}
		class="font-mono"
	/>
</FormField>

<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
	<Checkbox
		checked={(config.inherit_env as boolean) ?? true}
		disabled={readonly}
		onCheckedChange={(v) => onchange({ ...config, inherit_env: v })}
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
