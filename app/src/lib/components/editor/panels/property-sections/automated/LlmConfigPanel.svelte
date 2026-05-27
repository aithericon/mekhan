<script lang="ts">
	import CodeEditor from '../../shared/CodeEditor.svelte';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import InsertRefButton from '../InsertRefButton.svelte';
	import ResourcePicker from '../shared/ResourcePicker.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { untrack } from 'svelte';

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

	// Track the schema editor's raw text + last parse error so we can
	// surface "your schema is invalid JSON, the output fields are stale"
	// rather than swallowing the parse failure (which is what made the
	// editor look broken — typing a half-finished schema silently held
	// the derived fields at the last good shape).
	let schemaDraft = $state('');
	let schemaParseError = $state<string | null>(null);

	// Seed + resync the draft from the underlying config. We MUST only
	// react to config changes (response-format toggle, Yjs round-trip
	// from a remote peer, initial mount) — never to the user's own
	// keystrokes. Two guards:
	//
	// 1. `untrack` the schemaDraft read/write so the effect doesn't
	//    self-trigger every keystroke (which would reset the draft to
	//    the last *persisted* schema, making mid-edit JSON impossible
	//    to type one character at a time).
	// 2. Inside untrack: if the current draft already parses to the
	//    same content as `config.response_format.schema`, the change
	//    must be self-inflicted (the user just typed valid JSON, we
	//    propagated it, and it round-tripped back through Yjs). Skip
	//    the resync so the user's literal formatting + cursor position
	//    are preserved.
	$effect(() => {
		const schema =
			(config.response_format as Record<string, unknown>)?.schema ?? {};
		untrack(() => {
			// Empty draft (initial mount) → seed from config.
			if (schemaDraft === '') {
				schemaDraft = JSON.stringify(schema, null, 2);
				schemaParseError = null;
				return;
			}
			// Draft is mid-edit (doesn't parse) → don't overwrite the user's
			// in-flight typing with the last persisted shape.
			let draftParsed: unknown;
			try {
				draftParsed = JSON.parse(schemaDraft);
			} catch {
				return;
			}
			// Draft parses to the same content as the persisted schema →
			// self-inflicted round-trip; leave the draft alone to preserve
			// user formatting / cursor.
			if (JSON.stringify(draftParsed) === JSON.stringify(schema)) {
				schemaParseError = null;
				return;
			}
			// Real external change (format toggle, remote peer) → resync.
			schemaDraft = JSON.stringify(schema, null, 2);
			schemaParseError = null;
		});
	});

	// Heuristic for "schema parses but has no usable shape" — the
	// derive endpoint will fall back to a single text-mode `response`
	// field, which often surprises authors who expected their schema
	// to drive the output. Mirror the deriver's logic so the message
	// matches what they'll actually see.
	const schemaIsEffectivelyEmpty = $derived.by(() => {
		if (responseFormatType !== 'json_schema') return false;
		const schema = (config.response_format as Record<string, unknown>)?.schema as
			| Record<string, unknown>
			| undefined;
		if (!schema || Object.keys(schema).length === 0) return true;
		const t = schema.type as string | undefined;
		// Object schemas need `properties` to expand into per-field outputs.
		if (t === 'object') {
			const props = schema.properties as Record<string, unknown> | undefined;
			return !props || Object.keys(props).length === 0;
		}
		// Scalar/array schemas always derive a single `response` field
		// (handled by the backend). Anything with no `type` at all is
		// untyped → falls back to text mode.
		return t == null;
	});
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
			value={schemaDraft}
			language="json"
			{readonly}
			minHeight="80px"
			maxHeight="200px"
			onchange={(val) => {
				schemaDraft = val;
				try {
					const parsed = JSON.parse(val);
					schemaParseError = null;
					onchange({
						...config,
						response_format: {
							type: 'json_schema',
							schema: parsed
						}
					});
				} catch (e) {
					// Hold the last good schema (don't propagate) but
					// surface the parse error so the user knows the
					// derived output port is frozen until they fix it.
					schemaParseError = e instanceof Error ? e.message : String(e);
				}
			}}
		/>
		{#if schemaParseError}
			<p class="text-sm text-destructive" data-testid="llm-schema-parse-error">
				Invalid JSON — output fields won't update until this is fixed. ({schemaParseError})
			</p>
		{:else if schemaIsEffectivelyEmpty}
			<p class="text-sm text-muted-foreground" data-testid="llm-schema-empty-hint">
				Schema has no declared shape — output falls back to a single
				<code class="rounded bg-muted px-1 py-0.5 font-mono text-sm">response</code> field. Add
				<code class="rounded bg-muted px-1 py-0.5 font-mono text-sm">"type"</code> (e.g.
				<code class="rounded bg-muted px-1 py-0.5 font-mono text-sm">"string"</code>) for a single
				scalar output, or
				<code class="rounded bg-muted px-1 py-0.5 font-mono text-sm">"type":"object"</code> +
				<code class="rounded bg-muted px-1 py-0.5 font-mono text-sm">"properties"</code> to expand into
				multiple fields.
			</p>
		{/if}
	</div>
{/if}
