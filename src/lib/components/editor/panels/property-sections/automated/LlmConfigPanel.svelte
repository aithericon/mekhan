<script lang="ts">
	import CodeEditor from '../../shared/CodeEditor.svelte';
	import { Select, SelectTrigger, SelectContent, SelectItem } from '$lib/components/ui/select';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
	};

	let { config, readonly = false, onchange }: Props = $props();

	const providerLabels: Record<string, string> = {
		openai: 'OpenAI',
		anthropic: 'Anthropic',
		ollama: 'Ollama'
	};

	const responseFormatType = $derived(
		((config.response_format as Record<string, unknown>)?.type as string) ?? 'text'
	);
</script>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Provider</span>
	<Select.Root
		type="single"
		value={(config.provider as string) ?? 'openai'}
		onValueChange={(v) => { if (v) onchange({ ...config, provider: v }); }}
		disabled={readonly}
	>
		<SelectTrigger disabled={readonly}>
			{providerLabels[(config.provider as string) ?? 'openai'] ?? 'OpenAI'}
		</SelectTrigger>
		<SelectContent>
			<SelectItem value="openai" label="OpenAI" />
			<SelectItem value="anthropic" label="Anthropic" />
			<SelectItem value="ollama" label="Ollama" />
		</SelectContent>
	</Select.Root>
</div>

<div class="space-y-1.5">
	<label for="llm-model" class="text-xs font-medium text-muted-foreground">Model</label>
	<input
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
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
</div>

<div class="space-y-1.5">
	<label for="llm-api-key" class="text-xs font-medium text-muted-foreground"
		>API Key (optional)</label
	>
	<input
		id="llm-api-key"
		type="password"
		value={(config.api_key as string) ?? ''}
		placeholder="Falls back to env var"
		disabled={readonly}
		oninput={(e) =>
			onchange({ ...config, api_key: (e.currentTarget as HTMLInputElement).value || undefined })}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
</div>

<div class="space-y-1.5">
	<label for="llm-base-url" class="text-xs font-medium text-muted-foreground"
		>Base URL (optional)</label
	>
	<input
		id="llm-base-url"
		type="text"
		value={(config.base_url as string) ?? ''}
		placeholder="Custom endpoint or proxy"
		disabled={readonly}
		oninput={(e) =>
			onchange({ ...config, base_url: (e.currentTarget as HTMLInputElement).value || undefined })}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">System Prompt (optional)</span>
	<textarea
		value={(config.system_prompt as string) ?? ''}
		placeholder="System instructions..."
		disabled={readonly}
		oninput={(e) =>
			onchange({
				...config,
				system_prompt: (e.currentTarget as HTMLTextAreaElement).value || undefined
			})}
		rows={2}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	></textarea>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Prompt</span>
	<textarea
		value={(config.prompt as string) ?? ''}
		placeholder={'User prompt (supports {{variable}} templates)...'}
		disabled={readonly}
		oninput={(e) =>
			onchange({ ...config, prompt: (e.currentTarget as HTMLTextAreaElement).value })}
		rows={4}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	></textarea>
</div>

<div class="flex gap-3">
	<div class="flex-1 space-y-1.5">
		<label for="llm-temp" class="text-xs font-medium text-muted-foreground">Temperature</label>
		<input
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
			class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
	<div class="flex-1 space-y-1.5">
		<label for="llm-max-tokens" class="text-xs font-medium text-muted-foreground">Max Tokens</label
		>
		<input
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
			class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Response Format</span>
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
		<SelectTrigger disabled={readonly}>
			{responseFormatType === 'json_schema' ? 'JSON Schema' : 'Text'}
		</SelectTrigger>
		<SelectContent>
			<SelectItem value="text" label="Text" />
			<SelectItem value="json_schema" label="JSON Schema" />
		</SelectContent>
	</Select.Root>
</div>

{#if responseFormatType === 'json_schema'}
	<div class="space-y-1.5">
		<span class="text-xs font-medium text-muted-foreground">JSON Schema</span>
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
