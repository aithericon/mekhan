<script lang="ts">
	import KeyValueEditor from '../../shared/KeyValueEditor.svelte';
	import StringListEditor from '../../shared/StringListEditor.svelte';
	import * as Select from '$lib/components/ui/select';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
	};

	let { config, readonly = false, onchange }: Props = $props();

	const pullPolicyLabels: Record<string, string> = {
		if_not_present: 'If Not Present',
		always: 'Always',
		never: 'Never'
	};
</script>

<div class="space-y-1.5">
	<label for="docker-image" class="text-xs font-medium text-muted-foreground">Image</label>
	<input
		id="docker-image"
		type="text"
		value={(config.image as string) ?? ''}
		placeholder="e.g. python:3.12-slim"
		disabled={readonly}
		oninput={(e) => onchange({ ...config, image: (e.currentTarget as HTMLInputElement).value })}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Command</span>
	<StringListEditor
		items={(config.command as string[]) ?? []}
		{readonly}
		placeholder="e.g. python"
		onchange={(command) => onchange({ ...config, command })}
	/>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Entrypoint (optional)</span>
	<StringListEditor
		items={(config.entrypoint as string[]) ?? []}
		{readonly}
		placeholder="e.g. /bin/bash"
		onchange={(entrypoint) => onchange({ ...config, entrypoint })}
	/>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Pull Policy</span>
	<Select.Root
		type="single"
		value={(config.pull_policy as string) ?? 'if_not_present'}
		onValueChange={(v) => { if (v) onchange({ ...config, pull_policy: v }); }}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly}>
			{pullPolicyLabels[(config.pull_policy as string) ?? 'if_not_present'] ?? 'If Not Present'}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="if_not_present" label="If Not Present" />
			<Select.Item value="always" label="Always" />
			<Select.Item value="never" label="Never" />
		</Select.Content>
	</Select.Root>
</div>

<div class="space-y-1.5">
	<label for="network-mode" class="text-xs font-medium text-muted-foreground"
		>Network Mode (optional)</label
	>
	<input
		id="network-mode"
		type="text"
		value={(config.network_mode as string) ?? ''}
		placeholder="e.g. bridge, host, none"
		disabled={readonly}
		oninput={(e) =>
			onchange({ ...config, network_mode: (e.currentTarget as HTMLInputElement).value || undefined })}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
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

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Extra Volumes</span>
	<StringListEditor
		items={(config.extra_volumes as string[]) ?? []}
		{readonly}
		placeholder="host:container"
		onchange={(extra_volumes) => onchange({ ...config, extra_volumes })}
	/>
</div>

<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
	<input
		type="checkbox"
		checked={(config.remove_container as boolean) ?? true}
		disabled={readonly}
		onchange={(e) =>
			onchange({ ...config, remove_container: (e.currentTarget as HTMLInputElement).checked })}
		class="size-3.5 disabled:cursor-default disabled:opacity-70"
	/>
	Remove container after execution
</label>
