<script lang="ts" module>
	/**
	 * Shape both `LlmConfigPanel` (AutomatedStep flat config) and
	 * `AgentNodeSection` (nested `data.model.*` + `data.systemPrompt` +
	 * `data.userPrompt`) reduce to. The parents own the data shape;
	 * this component renders the LLM-author surface once and the parents
	 * adapt before/after.
	 *
	 * Scope: provider, model, resource picker, API key + base URL
	 * overrides, system prompt, user prompt. NOT response_format —
	 * `LlmConfigPanel` carries a Builder/Raw toggle wrapping its schema
	 * editor that the agent doesn't (yet) have; sharing the raw editor
	 * alone wasn't worth the seam.
	 *
	 * `userPrompt` is always present (LLM step's `prompt`, Agent's
	 * `userPrompt`). `systemPrompt` and the override `apiKey` / `baseUrl`
	 * are optional.
	 */
	export type LlmCommonShape = {
		provider: string;
		model: string;
		apiKey?: string;
		baseUrl?: string;
		resourceAlias?: string;
		systemPrompt?: string;
		userPrompt: string;
	};

	export const PROVIDER_LABELS: Record<string, string> = {
		openai: 'OpenAI',
		anthropic: 'Anthropic',
		ollama: 'Ollama'
	};

	/// Per-provider resource type map. Only `openai` has a workspace
	/// resource type today; `anthropic` / `ollama` fall back to manual
	/// api_key + base_url until those resource types ship.
	export const RESOURCE_TYPE_FOR_PROVIDER: Record<string, string | null> = {
		openai: 'openai',
		anthropic: null,
		ollama: null
	};
</script>

<script lang="ts">
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import InsertRefButton from '../InsertRefButton.svelte';
	import ResourcePicker from './ResourcePicker.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';

	type Props = {
		value: LlmCommonShape;
		onchange: (next: LlmCommonShape) => void;
		readonly?: boolean;
		scope?: ScopeEntry[];
		/// Label for the user-prompt textarea — LLM step calls it
		/// "Prompt", Agent calls it "User Prompt".
		userPromptLabel?: string;
		userPromptPlaceholder?: string;
		systemPromptPlaceholder?: string;
		/// Stable id prefix for input elements. Default `llm`; agent
		/// passes `agent` so testids and FormField for= attrs stay unique
		/// if both panels somehow render on the same screen.
		idPrefix?: string;
	};

	let {
		value,
		onchange,
		readonly = false,
		scope = [],
		userPromptLabel = 'Prompt',
		userPromptPlaceholder = 'User prompt (supports {{ upstream.field }} templates)…',
		systemPromptPlaceholder = 'System instructions…',
		idPrefix = 'llm'
	}: Props = $props();

	const provider = $derived(value.provider || 'openai');
	const resourceType = $derived(RESOURCE_TYPE_FOR_PROVIDER[provider] ?? null);
	const resourceAlias = $derived(value.resourceAlias ?? '');

	function patch(partial: Partial<LlmCommonShape>) {
		onchange({ ...value, ...partial });
	}

	function setResourceAlias(alias: string) {
		const next: LlmCommonShape = { ...value };
		if (alias) next.resourceAlias = alias;
		else delete next.resourceAlias;
		onchange(next);
	}

	function appendToField(field: 'systemPrompt' | 'userPrompt', snippet: string) {
		const curr = (value[field] as string | undefined) ?? '';
		patch({ [field]: curr ? `${curr} ${snippet}` : snippet } as Partial<LlmCommonShape>);
	}
</script>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Provider</span>
	<Select.Root
		type="single"
		value={provider}
		onValueChange={(v) => {
			if (v) patch({ provider: v });
		}}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly} data-testid="{idPrefix}-provider-select">
			{PROVIDER_LABELS[provider] ?? 'OpenAI'}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="openai" label="OpenAI" />
			<Select.Item value="anthropic" label="Anthropic" />
			<Select.Item value="ollama" label="Ollama" />
		</Select.Content>
	</Select.Root>
</div>

<FormField label="Model" for="{idPrefix}-model">
	<Input
		id="{idPrefix}-model"
		type="text"
		value={value.model ?? ''}
		placeholder={
			provider === 'anthropic'
				? 'claude-sonnet-4-20250514'
				: provider === 'ollama'
					? 'llama3'
					: 'gpt-4o'
		}
		disabled={readonly}
		oninput={(e) => patch({ model: (e.currentTarget as HTMLInputElement).value })}
		class="font-mono"
		data-testid="{idPrefix}-model-input"
	/>
</FormField>

<ResourcePicker
	{resourceType}
	selected={resourceAlias}
	onChange={setResourceAlias}
	label="Credentials resource"
	{readonly}
	testId="{idPrefix}-resource-select"
	typeLabel={PROVIDER_LABELS[provider]}
/>

<FormField
	label={resourceAlias ? 'API Key (override)' : 'API Key (optional)'}
	for="{idPrefix}-api-key"
>
	<Input
		id="{idPrefix}-api-key"
		type="password"
		value={value.apiKey ?? ''}
		placeholder={resourceAlias ? 'Inherits from resource' : 'Falls back to env var'}
		disabled={readonly}
		oninput={(e) =>
			patch({ apiKey: (e.currentTarget as HTMLInputElement).value || undefined })}
	/>
</FormField>

<FormField
	label={resourceAlias ? 'Base URL (override)' : 'Base URL (optional)'}
	for="{idPrefix}-base-url"
>
	<Input
		id="{idPrefix}-base-url"
		type="text"
		value={value.baseUrl ?? ''}
		placeholder={resourceAlias ? 'Inherits from resource' : 'Custom endpoint or proxy'}
		disabled={readonly}
		oninput={(e) =>
			patch({ baseUrl: (e.currentTarget as HTMLInputElement).value || undefined })}
		class="font-mono"
	/>
</FormField>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">System Prompt (optional)</span>
	<Textarea
		value={value.systemPrompt ?? ''}
		placeholder={systemPromptPlaceholder}
		disabled={readonly}
		oninput={(e) => {
			const v = (e.currentTarget as HTMLTextAreaElement).value;
			patch({ systemPrompt: v || undefined });
		}}
		rows={2}
		data-testid="{idPrefix}-system-prompt"
	/>
	{#if scope.length > 0}
		<InsertRefButton
			{scope}
			disabled={readonly}
			placeholder="Insert upstream ref…"
			oninsert={(snippet) => appendToField('systemPrompt', snippet)}
		/>
	{/if}
</div>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">{userPromptLabel}</span>
	<Textarea
		value={value.userPrompt}
		placeholder={userPromptPlaceholder}
		disabled={readonly}
		oninput={(e) => patch({ userPrompt: (e.currentTarget as HTMLTextAreaElement).value })}
		rows={4}
		data-testid="{idPrefix}-user-prompt"
	/>
	{#if scope.length > 0}
		<InsertRefButton
			{scope}
			disabled={readonly}
			placeholder="Insert upstream ref…"
			oninsert={(snippet) => appendToField('userPrompt', snippet)}
		/>
	{/if}
</div>
