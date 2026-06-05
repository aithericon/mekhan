<script lang="ts">
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import * as Select from '$lib/components/ui/select';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import { Input } from '$lib/components/ui/input';
	import CodeEditor from '$lib/components/editor/panels/shared/CodeEditor.svelte';
	import JsonSchemaBuilder, {
		detectShape
	} from '$lib/components/editor/panels/property-sections/automated/JsonSchemaBuilder.svelte';
	import InsertRefButton from '$lib/components/editor/panels/property-sections/InsertRefButton.svelte';
	import ModelPicker from '$lib/components/editor/panels/property-sections/shared/ModelPicker.svelte';
	import { untrack } from 'svelte';

	type Props = {
		binding: YjsGraphBinding;
		nodeId: string;
		readonly?: boolean;
		scope?: ScopeEntry[];
	};

	let { binding, nodeId, readonly = false, scope = [] }: Props = $props();

	const nodeData = $derived(
		binding.graph.nodes.find((n) => n.id === nodeId)?.data as AutomatedStepNodeData | null
	);
	const config = $derived<Record<string, unknown>>(
		(nodeData?.executionSpec.config as Record<string, unknown>) ?? {}
	);

	const provider = $derived((config.provider as string) ?? 'openai');
	/// GDPR: the internal pool routes through the in-cluster router — the model
	/// is picked from the loaded set, never free-typed.
	const isInternal = $derived(provider === 'internal');
	const resourceAlias = $derived((config.resource_alias as string) ?? '');
	const responseFormatType = $derived(
		((config.response_format as Record<string, unknown>)?.type as string) ?? 'text'
	);
	const schemaObj = $derived(
		((config.response_format as Record<string, unknown>)?.schema as Record<string, unknown>) ?? {}
	);
	const schemaShape = $derived(detectShape(schemaObj));
	const builderCompatible = $derived(schemaShape.kind !== 'raw_only');

	let schemaEditor = $state<'builder' | 'raw'>('builder');
	$effect(() => {
		const compatible = builderCompatible;
		untrack(() => {
			if (!compatible && schemaEditor === 'builder') schemaEditor = 'raw';
		});
	});

	let schemaDraft = $state('');
	let schemaParseError = $state<string | null>(null);
	$effect(() => {
		const schema = schemaObj;
		untrack(() => {
			if (schemaDraft === '') {
				schemaDraft = JSON.stringify(schema, null, 2);
				schemaParseError = null;
				return;
			}
			let parsed: unknown;
			try {
				parsed = JSON.parse(schemaDraft);
			} catch {
				return;
			}
			if (JSON.stringify(parsed) === JSON.stringify(schema)) {
				schemaParseError = null;
				return;
			}
			schemaDraft = JSON.stringify(schema, null, 2);
			schemaParseError = null;
		});
	});

	function updateConfig(next: Record<string, unknown>) {
		if (!nodeData) return;
		binding.updateNodeData(nodeId, {
			...nodeData,
			executionSpec: { ...nodeData.executionSpec, config: next }
		});
	}

	function setField(key: string, value: unknown) {
		updateConfig({ ...config, [key]: value });
	}

	function appendToField(field: 'prompt' | 'system_prompt', snippet: string) {
		const curr = (config[field] as string | undefined) ?? '';
		setField(field, curr ? `${curr} ${snippet}` : snippet);
	}

	function setResponseFormat(type: 'text' | 'json_schema') {
		if (type === 'text') {
			setField('response_format', { type: 'text' });
		} else {
			setField('response_format', { type: 'json_schema', schema: schemaObj });
		}
	}

	function handleBuilderChange(schema: Record<string, unknown>) {
		setField('response_format', { type: 'json_schema', schema });
	}

	const providerLabels: Record<string, string> = {
		openai: 'OpenAI',
		anthropic: 'Anthropic',
		ollama: 'Ollama',
		internal: 'Internal Model Pool'
	};
</script>

<div class="flex h-full flex-col">
	<div class="flex items-center border-b border-border bg-card px-4 py-2">
		<span class="text-sm font-semibold uppercase tracking-wider text-muted-foreground">
			LLM Step
		</span>
		{#if nodeData}
			<span class="ml-2 text-sm text-muted-foreground">— {nodeData.label}</span>
		{/if}
	</div>

	{#if nodeData}
		<div class="flex-1 overflow-y-auto p-6">
			<div class="mx-auto max-w-3xl space-y-5">
				<div class="flex gap-3">
					<FormField label="Provider" for="ide-llm-provider" class="flex-1">
						<Select.Root
							type="single"
							value={provider}
							onValueChange={(v) => {
								if (v) setField('provider', v);
							}}
							disabled={readonly}
						>
							<Select.Trigger disabled={readonly}>
								{providerLabels[provider] ?? 'OpenAI'}
							</Select.Trigger>
							<Select.Content>
								<Select.Item value="openai" label="OpenAI" />
								<Select.Item value="anthropic" label="Anthropic" />
								<Select.Item value="ollama" label="Ollama" />
								<Select.Item value="internal" label="Internal Model Pool" />
							</Select.Content>
						</Select.Root>
					</FormField>
					{#if isInternal}
						<div class="flex-1">
							<ModelPicker
								selected={(config.model as string) ?? ''}
								onChange={(modelId) => setField('model', modelId)}
								resourceAlias={resourceAlias}
								{readonly}
								testId="ide-llm-model-picker"
							/>
						</div>
					{:else}
						<FormField label="Model" for="ide-llm-model" class="flex-1">
							<Input
								id="ide-llm-model"
								type="text"
								value={(config.model as string) ?? ''}
								placeholder={provider === 'anthropic'
									? 'claude-sonnet-4-20250514'
									: provider === 'ollama'
										? 'llama3'
										: 'gpt-4o'}
								disabled={readonly}
								oninput={(e) =>
									setField('model', (e.currentTarget as HTMLInputElement).value)}
								class="font-mono"
							/>
						</FormField>
					{/if}
				</div>

				<div class="space-y-1.5">
					<span class="text-sm font-medium text-muted-foreground">System Prompt</span>
					<Textarea
						value={(config.system_prompt as string) ?? ''}
						placeholder="System instructions…"
						disabled={readonly}
						oninput={(e) =>
							setField('system_prompt', (e.currentTarget as HTMLTextAreaElement).value || undefined)}
						rows={8}
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
							setField('prompt', (e.currentTarget as HTMLTextAreaElement).value)}
						rows={14}
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

				<div class="space-y-1.5">
					<span class="text-sm font-medium text-muted-foreground">Response Format</span>
					<Select.Root
						type="single"
						value={responseFormatType}
						onValueChange={(v) => {
							if (v === 'text' || v === 'json_schema') setResponseFormat(v);
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
					<div class="space-y-2">
						<div class="flex items-center justify-between">
							<span class="text-sm font-medium text-muted-foreground">JSON Schema</span>
							<div class="flex gap-1">
								<button
									type="button"
									class="rounded-md border px-2 py-0.5 text-sm transition-colors {schemaEditor === 'builder'
										? 'border-primary bg-primary/5 text-foreground'
										: 'border-border text-muted-foreground hover:bg-accent/30'}"
									disabled={readonly || !builderCompatible}
									title={builderCompatible
										? 'Visual property editor'
										: 'Schema uses constructs the builder can’t represent — raw only.'}
									onclick={() => (schemaEditor = 'builder')}
								>
									Builder
								</button>
								<button
									type="button"
									class="rounded-md border px-2 py-0.5 text-sm transition-colors {schemaEditor === 'raw'
										? 'border-primary bg-primary/5 text-foreground'
										: 'border-border text-muted-foreground hover:bg-accent/30'}"
									disabled={readonly}
									onclick={() => (schemaEditor = 'raw')}
								>
									Raw JSON
								</button>
							</div>
						</div>

						{#if schemaEditor === 'builder' && builderCompatible}
							<JsonSchemaBuilder schema={schemaObj} {readonly} onchange={handleBuilderChange} />
						{:else}
							<CodeEditor
								value={schemaDraft}
								language="json"
								{readonly}
								minHeight="200px"
								maxHeight="500px"
								onchange={(val) => {
									schemaDraft = val;
									try {
										const parsed = JSON.parse(val);
										schemaParseError = null;
										setField('response_format', { type: 'json_schema', schema: parsed });
									} catch (e) {
										schemaParseError = e instanceof Error ? e.message : String(e);
									}
								}}
							/>
							{#if schemaParseError}
								<p class="text-sm text-destructive">
									Invalid JSON — output fields won't update until this is fixed. ({schemaParseError})
								</p>
							{/if}
						{/if}
					</div>
				{/if}
			</div>
		</div>
	{:else}
		<div class="flex flex-1 items-center justify-center text-sm text-muted-foreground">
			Node not found
		</div>
	{/if}
</div>
