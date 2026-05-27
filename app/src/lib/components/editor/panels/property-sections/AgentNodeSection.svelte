<script lang="ts">
	import type { AgentNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import CodeEditor from '../shared/CodeEditor.svelte';
	import InsertRefButton from './InsertRefButton.svelte';
	import ResourcePicker from './shared/ResourcePicker.svelte';
	import { untrack } from 'svelte';

	type Props = {
		data: AgentNodeData;
		readonly?: boolean;
		onchange: (data: AgentNodeData) => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
		scope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, binding, nodeId, scope = [] }: Props = $props();

	const providerLabels: Record<string, string> = {
		openai: 'OpenAI',
		anthropic: 'Anthropic',
		ollama: 'Ollama'
	};

	const resourceTypeForProvider: Record<string, string | null> = {
		openai: 'openai',
		anthropic: null,
		ollama: null
	};

	const provider = $derived(data.model?.provider ?? 'anthropic');
	const modelName = $derived(data.model?.model ?? '');
	const resourceAlias = $derived(data.model?.resourceAlias ?? '');
	const resourceType = $derived(resourceTypeForProvider[provider] ?? null);

	function updateModel<K extends keyof AgentNodeData['model']>(
		key: K,
		value: AgentNodeData['model'][K]
	) {
		onchange({ ...data, model: { ...data.model, [key]: value } });
	}

	function setResourceAlias(alias: string) {
		const next = { ...data.model } as AgentNodeData['model'];
		if (alias) {
			next.resourceAlias = alias;
		} else {
			delete (next as Record<string, unknown>).resourceAlias;
		}
		onchange({ ...data, model: next });
	}

	function appendToField(field: 'systemPrompt' | 'userPrompt', snippet: string) {
		const curr = (data[field] as string | undefined) ?? '';
		onchange({ ...data, [field]: curr ? `${curr} ${snippet}` : snippet });
	}

	// Response format: identical UX to LlmConfigPanel — text vs json_schema,
	// with the same draft/parse guard so mid-edit JSON isn't clobbered.
	const responseFormat = $derived(
		(data.responseFormat as Record<string, unknown> | undefined) ?? { type: 'text' }
	);
	const responseFormatType = $derived((responseFormat.type as string) ?? 'text');

	let schemaDraft = $state('');
	let schemaParseError = $state<string | null>(null);

	$effect(() => {
		const schema = (responseFormat.schema as Record<string, unknown> | undefined) ?? {};
		untrack(() => {
			if (schemaDraft === '') {
				schemaDraft = JSON.stringify(schema, null, 2);
				schemaParseError = null;
				return;
			}
			let draftParsed: unknown;
			try {
				draftParsed = JSON.parse(schemaDraft);
			} catch {
				return;
			}
			if (JSON.stringify(draftParsed) === JSON.stringify(schema)) {
				schemaParseError = null;
				return;
			}
			schemaDraft = JSON.stringify(schema, null, 2);
			schemaParseError = null;
		});
	});

	// Tool children: nodes in the same graph whose parent_id points here and
	// which carry `tool_meta`. Shown read-only — to edit a tool's name /
	// description the author opens that child node's panel. Compile rejects
	// duplicates on publish, so we surface that warning inline too.
	const toolChildren = $derived.by(() => {
		if (!binding || !nodeId) return [];
		return binding.graph.nodes.filter((n) => n.parentId === nodeId && n.toolMeta);
	});
	const duplicateToolNames = $derived.by(() => {
		const seen = new Set<string>();
		const dups = new Set<string>();
		for (const child of toolChildren) {
			const name = child.toolMeta?.toolName ?? '';
			if (!name) continue;
			if (seen.has(name)) dups.add(name);
			seen.add(name);
		}
		return dups;
	});

	// stop_when / max_turns warnings: a single-turn agent that publishes will
	// take the byte-identical AutomatedStep(Llm) path. Surface that so the
	// author knows tools won't fire — and the agent loop won't either.
	const isSingleShot = $derived((data.maxTurns ?? 1) <= 1 && !data.stopWhen);
</script>

<!-- Model -->
<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Provider</span>
	<Select.Root
		type="single"
		value={provider}
		onValueChange={(v) => {
			if (v) updateModel('provider', v);
		}}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly} data-testid="agent-provider-select">
			{providerLabels[provider] ?? 'Anthropic'}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="openai" label="OpenAI" />
			<Select.Item value="anthropic" label="Anthropic" />
			<Select.Item value="ollama" label="Ollama" />
		</Select.Content>
	</Select.Root>
</div>

<FormField label="Model" for="agent-model">
	<Input
		id="agent-model"
		type="text"
		value={modelName}
		placeholder={
			provider === 'anthropic'
				? 'claude-haiku-4-5-20251001'
				: provider === 'ollama'
					? 'llama3'
					: 'gpt-4o'
		}
		disabled={readonly}
		oninput={(e) => updateModel('model', (e.currentTarget as HTMLInputElement).value)}
		class="font-mono"
		data-testid="agent-model-input"
	/>
</FormField>

<ResourcePicker
	{resourceType}
	selected={resourceAlias}
	onChange={setResourceAlias}
	label="Credentials resource"
	{readonly}
	testId="agent-resource-select"
	typeLabel={providerLabels[provider]}
/>

<!-- Prompts -->
<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">System Prompt (optional)</span>
	<Textarea
		value={data.systemPrompt ?? ''}
		placeholder="You are a helpful agent. Call tools when needed and stop when finished."
		disabled={readonly}
		oninput={(e) => {
			const v = (e.currentTarget as HTMLTextAreaElement).value;
			onchange({ ...data, systemPrompt: v || undefined });
		}}
		rows={3}
		data-testid="agent-system-prompt"
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
	<span class="text-sm font-medium text-muted-foreground">User Prompt</span>
	<Textarea
		value={data.userPrompt}
		placeholder={'Initial turn prompt. Supports {{ upstream.field }} placeholders.'}
		disabled={readonly}
		oninput={(e) =>
			onchange({ ...data, userPrompt: (e.currentTarget as HTMLTextAreaElement).value })}
		rows={4}
		data-testid="agent-user-prompt"
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

<!-- Loop controls -->
<div class="flex gap-3">
	<FormField label="Max turns" for="agent-max-turns" class="flex-1">
		<Input
			id="agent-max-turns"
			type="number"
			min={1}
			value={data.maxTurns ?? 1}
			disabled={readonly}
			oninput={(e) => {
				const val = parseInt((e.currentTarget as HTMLInputElement).value);
				onchange({ ...data, maxTurns: isNaN(val) || val < 1 ? 1 : val });
			}}
			data-testid="agent-max-turns"
		/>
	</FormField>
	<FormField label="Temperature" for="agent-temp" class="flex-1">
		<Input
			id="agent-temp"
			type="number"
			min={0}
			max={2}
			step={0.1}
			value={(data.model?.temperature as number | null | undefined) ?? ''}
			placeholder="Default"
			disabled={readonly}
			oninput={(e) => {
				const v = parseFloat((e.currentTarget as HTMLInputElement).value);
				updateModel('temperature', isNaN(v) ? null : v);
			}}
		/>
	</FormField>
</div>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Stop when (optional)</span>
	<Textarea
		value={data.stopWhen ?? ''}
		placeholder={'Rhai over agent state, e.g. state.final_response != ()'}
		disabled={readonly}
		oninput={(e) => {
			const v = (e.currentTarget as HTMLTextAreaElement).value;
			onchange({ ...data, stopWhen: v || undefined });
		}}
		rows={2}
		class="font-mono text-sm"
		data-testid="agent-stop-when"
	/>
	<p class="text-sm text-muted-foreground">
		Evaluates on the parked agent state after every turn. <code>state.turn</code>,
		<code>state.final_response</code>, <code>state.message_count</code>,
		<code>state.total_tokens_in</code>, <code>state.total_tokens_out</code> are in scope.
	</p>
</div>

<!-- Policies -->
<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">On tool error</span>
	<Select.Root
		type="single"
		value={data.onToolError ?? 'feedback'}
		onValueChange={(v) => {
			if (v === 'feedback' || v === 'bubble') onchange({ ...data, onToolError: v });
		}}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly} data-testid="agent-on-tool-error">
			{(data.onToolError ?? 'feedback') === 'feedback' ? 'Feedback to model' : 'Bubble to error'}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="feedback" label="Feedback to model" />
			<Select.Item value="bubble" label="Bubble to error output" />
		</Select.Content>
	</Select.Root>
	<p class="text-sm text-muted-foreground">
		{(data.onToolError ?? 'feedback') === 'feedback'
			? 'Tool failure becomes a tool-role message; the model retries on the next turn.'
			: "Tool failure dead-ends the agent's success path and exits via the error handle."}
	</p>
</div>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Context strategy</span>
	<Select.Root
		type="single"
		value={data.contextStrategy ?? 'none'}
		onValueChange={(v) => {
			if (v === 'none' || v === 'drop_oldest' || v === 'summarize_oldest') {
				onchange({ ...data, contextStrategy: v });
			}
		}}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly} data-testid="agent-context-strategy">
			{
				data.contextStrategy === 'drop_oldest'
					? 'Drop oldest (preview)'
					: data.contextStrategy === 'summarize_oldest'
						? 'Summarize oldest (preview)'
						: 'None — keep full history'
			}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="none" label="None — keep full history" />
			<Select.Item value="drop_oldest" label="Drop oldest (preview)" />
			<Select.Item value="summarize_oldest" label="Summarize oldest (preview)" />
		</Select.Content>
	</Select.Root>
	{#if data.contextStrategy && data.contextStrategy !== 'none'}
		<p class="text-sm text-destructive" data-testid="agent-context-strategy-warning">
			Publish will reject — only <code>none</code> is implemented in v1.
		</p>
	{/if}
</div>

<!-- Response format -->
<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Response format</span>
	<Select.Root
		type="single"
		value={responseFormatType}
		onValueChange={(v) => {
			if (!v) return;
			if (v === 'text') {
				onchange({ ...data, responseFormat: { type: 'text' } });
			} else {
				onchange({
					...data,
					responseFormat: {
						type: 'json_schema',
						schema: responseFormat.schema ?? {}
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
						...data,
						responseFormat: { type: 'json_schema', schema: parsed }
					});
				} catch (e) {
					schemaParseError = e instanceof Error ? e.message : String(e);
				}
			}}
		/>
		{#if schemaParseError}
			<p class="text-sm text-destructive">
				Invalid JSON — schema is frozen at the last valid value. ({schemaParseError})
			</p>
		{/if}
	</div>
{/if}

<!-- Tool children summary -->
<div class="space-y-2 border-t border-border/40 pt-3">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Tools</span>
		<span class="text-sm text-muted-foreground">
			{toolChildren.length} child{toolChildren.length === 1 ? '' : 'ren'}
		</span>
	</div>
	{#if isSingleShot}
		<p class="text-sm text-muted-foreground" data-testid="agent-single-shot-hint">
			Single-turn agent — compiles to a plain LLM step and tool children are inert. Raise
			<code>Max turns</code> or set <code>Stop when</code> to enable the agent loop.
		</p>
	{:else if toolChildren.length === 0}
		<p class="text-sm text-muted-foreground">
			No tool children yet. Drag an Automated Step (or any node type) onto this agent on the
			canvas, then tag it with a tool name in its panel.
		</p>
	{:else}
		<ul class="space-y-1" data-testid="agent-tool-list">
			{#each toolChildren as child (child.id)}
				<li class="flex items-start justify-between gap-2 rounded-md border border-border/60 px-2 py-1.5">
					<div class="min-w-0 flex-1">
						<div class="flex items-center gap-1.5">
							<code class="truncate font-mono text-sm font-medium text-foreground">
								{child.toolMeta?.toolName || '<unnamed>'}
							</code>
							{#if duplicateToolNames.has(child.toolMeta?.toolName ?? '')}
								<span
									class="rounded bg-destructive/10 px-1.5 py-0.5 text-sm font-medium text-destructive"
									data-testid="agent-tool-duplicate"
								>
									duplicate
								</span>
							{/if}
						</div>
						{#if child.toolMeta?.toolDescription}
							<p class="truncate text-sm text-muted-foreground" title={child.toolMeta.toolDescription}>
								{child.toolMeta.toolDescription}
							</p>
						{/if}
					</div>
					<span class="shrink-0 text-sm text-muted-foreground/70">{child.type}</span>
				</li>
			{/each}
		</ul>
	{/if}
</div>
