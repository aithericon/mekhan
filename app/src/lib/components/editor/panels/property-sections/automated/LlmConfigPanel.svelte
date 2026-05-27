<script lang="ts">
	import CodeEditor from '../../shared/CodeEditor.svelte';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';
	import JsonSchemaBuilder, { detectShape } from './JsonSchemaBuilder.svelte';
	import LlmCommonFields, {
		type LlmCommonShape
	} from '../shared/LlmCommonFields.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { untrack } from 'svelte';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
		scope?: ScopeEntry[];
		nodeId?: string;
		templateId?: string;
	};

	let { config, readonly = false, onchange, scope = [], nodeId, templateId }: Props = $props();

	function ideHref(): string | null {
		if (!templateId || !nodeId) return null;
		const params = new URLSearchParams({ node: nodeId });
		return `/templates/${templateId}/ide?${params.toString()}`;
	}

	// Project the flat AutomatedStep config into the shared shape and
	// back. Empty/undefined values delete the key so opt-out semantics
	// round-trip through Yjs unchanged.
	const common = $derived<LlmCommonShape>({
		provider: (config.provider as string) ?? 'openai',
		model: (config.model as string) ?? '',
		apiKey: config.api_key as string | undefined,
		baseUrl: config.base_url as string | undefined,
		resourceAlias: config.resource_alias as string | undefined,
		systemPrompt: config.system_prompt as string | undefined,
		userPrompt: (config.prompt as string) ?? ''
	});

	function applyCommon(next: LlmCommonShape) {
		const out: Record<string, unknown> = { ...config };
		out.provider = next.provider;
		out.model = next.model;
		out.prompt = next.userPrompt;
		setOrDelete(out, 'api_key', next.apiKey);
		setOrDelete(out, 'base_url', next.baseUrl);
		setOrDelete(out, 'resource_alias', next.resourceAlias);
		setOrDelete(out, 'system_prompt', next.systemPrompt);
		onchange(out);
	}

	function setOrDelete(out: Record<string, unknown>, key: string, value: unknown) {
		if (value === undefined || value === '' || value === null) {
			delete out[key];
		} else {
			out[key] = value;
		}
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

	const schemaObj = $derived(
		((config.response_format as Record<string, unknown>)?.schema as Record<string, unknown>) ?? {}
	);
	const schemaShape = $derived(detectShape(schemaObj));
	const builderCompatible = $derived(schemaShape.kind !== 'raw_only');

	// Builder vs raw JSON. Default to builder when the persisted schema is
	// round-trippable (multi-field object or root scalar). When it isn't,
	// fall back to raw + disable the Builder toggle so the user doesn't lose
	// the bits the builder can't represent.
	let schemaEditor = $state<'builder' | 'raw'>('builder');
	$effect(() => {
		const compatible = builderCompatible;
		untrack(() => {
			if (!compatible && schemaEditor === 'builder') schemaEditor = 'raw';
		});
	});

	function handleBuilderChange(schema: Record<string, unknown>) {
		onchange({
			...config,
			response_format: { type: 'json_schema', schema }
		});
	}

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

{#if ideHref()}
	<a
		href={ideHref()}
		class="flex items-center justify-center gap-1.5 rounded-md border border-dashed border-border py-1.5 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
		title="Edit prompts and schema with more room"
		data-testid="llm-open-in-ide"
	>
		Open in IDE
	</a>
{/if}

<LlmCommonFields
	value={common}
	onchange={applyCommon}
	{readonly}
	{scope}
	userPromptLabel="Prompt"
	idPrefix="llm"
/>

<!-- LLM AutomatedStep-only: temperature + max_tokens side by side. -->
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
	<div class="space-y-2">
		<div class="flex items-center justify-between">
			<span class="text-sm font-medium text-muted-foreground">JSON Schema</span>
			<div class="flex gap-1" data-testid="llm-schema-editor-toggle">
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
					data-testid="llm-schema-mode-builder"
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
					data-testid="llm-schema-mode-raw"
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
		{/if}
	</div>
{/if}
