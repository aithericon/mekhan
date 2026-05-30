<script lang="ts">
	import type { AgentNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import CodeEditor from '../shared/CodeEditor.svelte';
	import LlmCommonFields, {
		type LlmCommonShape
	} from './shared/LlmCommonFields.svelte';
	import JsonSchemaBuilder, { detectShape } from './automated/JsonSchemaBuilder.svelte';
	import DeploymentSection from './DeploymentSection.svelte';
	import { sanitizeSlug } from '$lib/editor/sanitize-slug';
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

	// Project the agent's nested `data.model.*` + `data.systemPrompt` +
	// `data.userPrompt` into the shared `LlmCommonShape` and back. The
	// shared component knows nothing about whether values live flat or
	// nested; the adapter handles the translation.
	const common = $derived<LlmCommonShape>({
		provider: data.model?.provider ?? 'anthropic',
		model: data.model?.model ?? '',
		apiKey: data.model?.apiKey ?? undefined,
		baseUrl: data.model?.baseUrl ?? undefined,
		resourceAlias: data.model?.resourceAlias ?? undefined,
		systemPrompt: data.systemPrompt ?? undefined,
		userPrompt: data.userPrompt
	});

	function applyCommon(next: LlmCommonShape) {
		const nextModel = { ...data.model } as AgentNodeData['model'];
		nextModel.provider = next.provider;
		nextModel.model = next.model;
		setOrDelete(nextModel as Record<string, unknown>, 'apiKey', next.apiKey);
		setOrDelete(nextModel as Record<string, unknown>, 'baseUrl', next.baseUrl);
		setOrDelete(nextModel as Record<string, unknown>, 'resourceAlias', next.resourceAlias);
		const nextData: AgentNodeData = { ...data, model: nextModel, userPrompt: next.userPrompt };
		if (next.systemPrompt) {
			nextData.systemPrompt = next.systemPrompt;
		} else {
			delete (nextData as Record<string, unknown>).systemPrompt;
		}
		onchange(nextData);
	}

	function setOrDelete(out: Record<string, unknown>, key: string, value: unknown) {
		if (value === undefined || value === '' || value === null) {
			delete out[key];
		} else {
			out[key] = value;
		}
	}

	// Response format: text vs json_schema, with a Builder/Raw toggle for the
	// schema (same component the retired LLM step used — single source of
	// truth via the shared `JsonSchemaBuilder`). The draft/parse guard keeps
	// mid-edit JSON from being clobbered by a Yjs round-trip.
	const responseFormat = $derived(
		(data.responseFormat as Record<string, unknown> | undefined) ?? { type: 'text' }
	);
	const responseFormatType = $derived((responseFormat.type as string) ?? 'text');
	const schemaObj = $derived(
		(responseFormat.schema as Record<string, unknown> | undefined) ?? {}
	);
	const schemaShape = $derived(detectShape(schemaObj));
	const builderCompatible = $derived(schemaShape.kind !== 'raw_only');

	let schemaDraft = $state('');
	let schemaParseError = $state<string | null>(null);

	// Builder vs raw JSON. Default to builder when the persisted schema is
	// round-trippable; fall back to raw + disable the toggle when it isn't.
	let schemaEditor = $state<'builder' | 'raw'>('builder');
	$effect(() => {
		const compatible = builderCompatible;
		untrack(() => {
			if (!compatible && schemaEditor === 'builder') schemaEditor = 'raw';
		});
	});

	function handleBuilderChange(schema: Record<string, unknown>) {
		onchange({ ...data, responseFormat: { type: 'json_schema', schema } });
	}

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

	// Tool children: every node reachable from this agent via a
	// `tools`-handle edge. The agent's `tools` source handle is the
	// binding mechanism — drag from it onto any node you want the LLM
	// to be able to call. The LLM-facing tool name + description come
	// straight from the target node's own `label` and `description` (no
	// separate `toolMeta` field — single source of truth). To edit them,
	// open the target node's panel.
	const toolChildren = $derived.by(() => {
		if (!binding || !nodeId) return [];
		const nodeById = new Map(binding.graph.nodes.map((n) => [n.id, n]));
		const targets: typeof binding.graph.nodes = [];
		for (const e of binding.graph.edges) {
			if (e.source !== nodeId) continue;
			if (e.sourceHandle !== 'tools') continue;
			const t = nodeById.get(e.target);
			if (t) targets.push(t);
		}
		return targets;
	});
	// Helpers: tool name = slugified node label; tool description = node
	// description verbatim. Mirrors the compiler's derivation in
	// `service/src/compiler/lower/agent.rs`. We surface BOTH the raw label
	// and the slugified form in the panel so authors see exactly what the
	// LLM will call.
	type ToolLike = { data?: { label?: string | null; description?: string | null } };
	function toolNameFor(child: ToolLike): string {
		const label = child.data?.label?.trim() ?? '';
		if (!label) return '';
		return sanitizeSlug(label);
	}
	function toolDescriptionFor(child: ToolLike): string {
		return child.data?.description?.trim() ?? '';
	}

	const duplicateToolNames = $derived.by(() => {
		const seen = new Set<string>();
		const dups = new Set<string>();
		for (const child of toolChildren) {
			const name = toolNameFor(child);
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

	// Deployment: single-shot agents inherit the full AutomatedStep dispatch
	// (pool / scheduler / lease) via the byte-identical degenerate lowering.
	// Multi-turn / tool-bearing agents run their turns on the plain executor
	// pool only in v1 — a non-default deployment there is compile-rejected, so
	// warn in the editor (same idiom as the context-strategy preview warning).
	const deploymentIsDefault = $derived(
		!data.deploymentModel ||
			(data.deploymentModel.mode === 'executor' && data.deploymentModel.pool == null)
	);
	const deploymentRejectedForLoop = $derived(!isSingleShot && !deploymentIsDefault);
</script>

<LlmCommonFields
	value={common}
	onchange={applyCommon}
	{readonly}
	{scope}
	userPromptLabel="User Prompt"
	userPromptPlaceholder={'Initial turn prompt. Supports {{ upstream.field }} placeholders.'}
	systemPromptPlaceholder="You are a helpful agent. Call tools when needed and stop when finished."
	idPrefix="agent"
/>

<!-- Loop controls: max_turns + temperature side by side (LLM step
     uses max_tokens here instead). -->
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
				onchange({ ...data, model: { ...data.model, temperature: isNaN(v) ? null : v } });
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
	<div class="space-y-2">
		<div class="flex items-center justify-between">
			<span class="text-sm font-medium text-muted-foreground">JSON Schema</span>
			<div class="flex gap-1" data-testid="agent-schema-editor-toggle">
				<button
					type="button"
					class="rounded-md border px-2 py-0.5 text-sm transition-colors {schemaEditor ===
					'builder'
						? 'border-primary bg-primary/5 text-foreground'
						: 'border-border text-muted-foreground hover:bg-accent/30'}"
					disabled={readonly || !builderCompatible}
					title={builderCompatible
						? 'Visual property editor'
						: 'Schema uses constructs the builder can’t represent — raw only.'}
					onclick={() => (schemaEditor = 'builder')}
					data-testid="agent-schema-mode-builder"
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
					data-testid="agent-schema-mode-raw"
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
		{/if}
	</div>
{/if}

<!-- Deployment: where each inference turn runs. Single-shot agents reach the
     full Executor{pool} / Scheduled{lease} dispatch; multi-turn agents run on
     the plain executor pool only in v1 (a non-default choice is compile-
     rejected — surfaced below). -->
<DeploymentSection
	value={data.deploymentModel}
	{readonly}
	onchange={(dm) => onchange({ ...data, deploymentModel: dm })}
/>
{#if deploymentRejectedForLoop}
	<p class="text-sm text-destructive" data-testid="agent-deployment-loop-warning">
		Publish will reject — pooled/scheduled deployment is only supported on single-shot agents
		in v1. Set <code>Max turns</code> to 1 and clear <code>Stop when</code> / tools, or use the
		<code>Executor (worker pool)</code> default here.
	</p>
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
			No tools connected yet. Drag from the agent's <code>tools</code> handle (top of the node)
			to any Automated Step or SubWorkflow you want the LLM to be able to call — the model picks
			tools by name from this list each turn.
		</p>
	{:else}
		<p class="text-sm text-muted-foreground">
			The model picks one of these by name each turn. Click a tool below to open its panel and
			set its name + description.
		</p>
		<ul class="space-y-1" data-testid="agent-tool-list">
			{#each toolChildren as child (child.id)}
				{@const tName = toolNameFor(child)}
				{@const tDesc = toolDescriptionFor(child)}
				{@const hasName = tName !== ''}
				{@const isDup = hasName && duplicateToolNames.has(tName)}
				<li class="flex items-start justify-between gap-2 rounded-md border border-border/60 px-2 py-1.5">
					<div class="min-w-0 flex-1">
						<div class="flex items-center gap-1.5">
							{#if hasName}
								<code class="truncate font-mono text-sm font-medium text-foreground">
									{tName}
								</code>
							{:else}
								<span class="truncate text-sm text-muted-foreground italic">
									{child.data?.label ?? child.id}
								</span>
								<span
									class="shrink-0 rounded bg-amber-500/15 px-1.5 py-0.5 text-sm font-medium text-amber-700 dark:text-amber-400"
									title="The tool name is derived from this node's label (slugified). An empty label means the LLM can't see this tool — publish will reject it."
									data-testid="agent-tool-needs-name"
								>
									needs label
								</span>
							{/if}
							{#if isDup}
								<span
									class="shrink-0 rounded bg-destructive/10 px-1.5 py-0.5 text-sm font-medium text-destructive"
									data-testid="agent-tool-duplicate"
								>
									duplicate
								</span>
							{/if}
						</div>
						{#if tDesc}
							<p class="truncate text-sm text-muted-foreground" title={tDesc}>
								{tDesc}
							</p>
						{:else if hasName}
							<p class="truncate text-sm text-muted-foreground italic">
								(no description — set one on the tool node so the model knows when to call it)
							</p>
						{/if}
					</div>
					<span class="shrink-0 text-sm text-muted-foreground/70">{child.type}</span>
				</li>
			{/each}
		</ul>
	{/if}
</div>
