<script lang="ts">
	import CodeEditor from '../../shared/CodeEditor.svelte';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import InsertRefButton from '../InsertRefButton.svelte';
	import ResourcePicker from '../shared/ResourcePicker.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
		scope?: ScopeEntry[];
	};

	let { config, readonly = false, onchange, scope = [] }: Props = $props();

	// Per-provider resource type map — resource pickers are provider-scoped.
	// Only `openai` has a workspace resource type today; `anthropic` / `ollama`
	// fall back to manual api_key + base_url until those resource types ship.
	const resourceTypeForProvider: Record<string, string | null> = {
		openai: 'openai',
		anthropic: null,
		ollama: null
	};

	const providerLabels: Record<string, string> = {
		openai: 'OpenAI',
		anthropic: 'Anthropic',
		ollama: 'Ollama'
	};

	const provider = $derived((config.provider as string) ?? 'openai');
	const resourceType = $derived(resourceTypeForProvider[provider] ?? null);
	const resourceAlias = $derived((config.resource_alias as string | undefined) ?? '');

	function appendToField(field: 'prompt' | 'system_prompt', snippet: string) {
		const curr = (config[field] as string | undefined) ?? '';
		onchange({
			...config,
			[field]: curr ? `${curr} ${snippet}` : snippet
		});
	}

	function setResourceAlias(alias: string) {
		const next: Record<string, unknown> = { ...config };
		if (alias) {
			next.resource_alias = alias;
		} else {
			delete next.resource_alias;
		}
		onchange(next);
	}

	const responseFormatType = $derived(
		((config.response_format as Record<string, unknown>)?.type as string) ?? 'text'
	);
</script>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Provider</span>
	<Select.Root
		type="single"
		value={(config.provider as string) ?? 'openai'}
		onValueChange={(v) => { if (v) onchange({ ...config, provider: v }); }}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly}>
			{providerLabels[(config.provider as string) ?? 'openai'] ?? 'OpenAI'}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="openai" label="OpenAI" />
			<Select.Item value="anthropic" label="Anthropic" />
			<Select.Item value="ollama" label="Ollama" />
		</Select.Content>
	</Select.Root>
</div>

<FormField label="Model" for="llm-model">
	<Input
		id="llm-model"
		type="text"
		value={(config.model as string) ?? ''}
		placeholder={
			(config.provider as string) === 'anthropic'
				? 'claude-sonnet-4-20250514'
				: (config.provider as string) === 'ollama'
					? 'llama3'
					: 'gpt-4o'
		}
		disabled={readonly}
		oninput={(e) => onchange({ ...config, model: (e.currentTarget as HTMLInputElement).value })}
		class="font-mono"
	/>
</FormField>

<ResourcePicker
	{resourceType}
	selected={resourceAlias}
	onChange={setResourceAlias}
	label="Credentials resource"
	{readonly}
	testId="llm-resource-select"
	typeLabel={providerLabels[provider]}
/>

<FormField
	label={resourceAlias ? 'API Key (override)' : 'API Key (optional)'}
	for="llm-api-key"
>
	<Input
		id="llm-api-key"
		type="password"
		value={(config.api_key as string) ?? ''}
		placeholder={resourceAlias ? 'Inherits from resource' : 'Falls back to env var'}
		disabled={readonly}
		oninput={(e) =>
			onchange({ ...config, api_key: (e.currentTarget as HTMLInputElement).value || undefined })}
	/>
</FormField>

<FormField
	label={resourceAlias ? 'Base URL (override)' : 'Base URL (optional)'}
	for="llm-base-url"
>
	<Input
		id="llm-base-url"
		type="text"
		value={(config.base_url as string) ?? ''}
		placeholder={resourceAlias ? 'Inherits from resource' : 'Custom endpoint or proxy'}
		disabled={readonly}
		oninput={(e) =>
			onchange({ ...config, base_url: (e.currentTarget as HTMLInputElement).value || undefined })}
		class="font-mono"
	/>
</FormField>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">System Prompt (optional)</span>
	<Textarea
		value={(config.system_prompt as string) ?? ''}
		placeholder="System instructions..."
		disabled={readonly}
		oninput={(e) =>
			onchange({
				...config,
				system_prompt: (e.currentTarget as HTMLTextAreaElement).value || undefined
			})}
		rows={2}
	/>
	{#if scope.length > 0}
		<InsertRefButton
			{scope}
			disabled={readonly}
			placeholder="Insert upstream ref…"
			oninsert={(snippet) => appendToField('system_prompt', snippet)}
		/>
	{/if}
</div>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Prompt</span>
	<Textarea
		value={(config.prompt as string) ?? ''}
		placeholder={'User prompt (supports {{ upstream.field }} templates)…'}
		disabled={readonly}
		oninput={(e) =>
			onchange({ ...config, prompt: (e.currentTarget as HTMLTextAreaElement).value })}
		rows={4}
	/>
	{#if scope.length > 0}
		<InsertRefButton
			{scope}
			disabled={readonly}
			placeholder="Insert upstream ref…"
			oninsert={(snippet) => appendToField('prompt', snippet)}
		/>
	{/if}
</div>

<div class="flex gap-3">
	<FormField label="Temperature" for="llm-temp" class="flex-1">
		<Input
			id="llm-temp"
			type="number"
			min={0}
			max={2}
			step={0.1}
			value={(config.temperature as number) ?? ''}
			placeholder="Default"
			disabled={readonly}
			oninput={(e) => {
				const val = parseFloat((e.currentTarget as HTMLInputElement).value);
				onchange({ ...config, temperature: isNaN(val) ? undefined : val });
			}}
		/>
	</FormField>
	<FormField label="Max Tokens" for="llm-max-tokens" class="flex-1">
		<Input
			id="llm-max-tokens"
			type="number"
			min={1}
			value={(config.max_tokens as number) ?? ''}
			placeholder="Default"
			disabled={readonly}
			oninput={(e) => {
				const val = parseInt((e.currentTarget as HTMLInputElement).value);
				onchange({ ...config, max_tokens: isNaN(val) ? undefined : val });
			}}
		/>
	</FormField>
</div>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Response Format</span>
	<Select.Root
		type="single"
		value={responseFormatType}
		onValueChange={(v) => {
			if (!v) return;
			if (v === 'text') {
				onchange({ ...config, response_format: { type: 'text' } });
			} else {
				onchange({
					...config,
					response_format: {
						type: 'json_schema',
						schema: (config.response_format as Record<string, unknown>)?.schema ?? {}
					}
				});
			}
		}}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly}>
			{responseFormatType === 'json_schema' ? 'JSON Schema' : 'Text'}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="text" label="Text" />
			<Select.Item value="json_schema" label="JSON Schema" />
		</Select.Content>
	</Select.Root>
</div>

{#if responseFormatType === 'json_schema'}
	<div class="space-y-1.5">
		<span class="text-sm font-medium text-muted-foreground">JSON Schema</span>
		<CodeEditor
			value={JSON.stringify(
				(config.response_format as Record<string, unknown>)?.schema ?? {},
				null,
				2
			)}
			language="json"
			{readonly}
			minHeight="80px"
			maxHeight="200px"
			onchange={(val) => {
				try {
					onchange({
						...config,
						response_format: {
							type: 'json_schema',
							schema: JSON.parse(val)
						}
					});
				} catch {
					// Wait for valid JSON
				}
			}}
		/>
	</div>
{/if}
