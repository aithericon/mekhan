<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import Markdown from './Markdown.svelte';
	import SmartValue from './SmartValue.svelte';
	import type { RendererProps } from './types';

	// Canonical LLM output shape from the executor-llm backend (see
	// `executor/crates/executor-llm/src/backend.rs:203-212`):
	//   { response: string|json, model: string, usage: { input_tokens,
	//     output_tokens, total_tokens }, finish_reason: string, ...user-declared }
	// The `response` field is the model's text output — it's the part the
	// user cares about, and it's nearly always markdown. KeyValueList
	// squeezes it into a constrained right column; we render it
	// prominently below a compact metadata strip instead.
	type Envelope = {
		response?: unknown;
		model?: string;
		finish_reason?: string;
		usage?: {
			input_tokens?: number;
			output_tokens?: number;
			total_tokens?: number;
		};
		[k: string]: unknown;
	};

	let { value, ctx }: RendererProps = $props();
	const env = $derived(value as Envelope);

	// Structured-response branch — JSON-typed LLM outputs cascade through
	// SmartValue so KeyValueList / TabularArray / JsonBlock (CodeMirror) pick
	// up the shape instead of getting dumped as a markdown-fenced blob.
	const responseIsString = $derived(typeof env.response === 'string');
	const responseDefined = $derived(env.response !== undefined && env.response !== null);

	const usage = $derived(env.usage);

	// Surface any user-declared output fields (the executor copies
	// `response_value` into every spec-declared output name; see backend.rs:215-219)
	// next to the canonical ones so the renderer doesn't hide them.
	const KNOWN_KEYS: ReadonlySet<string> = new Set([
		'response',
		'model',
		'finish_reason',
		'usage'
	]);
	const extras = $derived<Array<[string, unknown]>>(
		Object.entries(env).filter(([k]) => !KNOWN_KEYS.has(k))
	);
</script>

<div class="space-y-3">
	<div class="flex flex-wrap items-center gap-2 text-sm">
		{#if env.model}
			<Badge variant="secondary" class="font-mono">{env.model}</Badge>
		{/if}
		{#if env.finish_reason}
			{@const ok = env.finish_reason === 'stop' || env.finish_reason === 'end'}
			<Badge
				variant="outline"
				class="font-mono {ok ? '' : 'border-amber-300 bg-amber-50 text-amber-800'}"
				title={ok ? 'Model finished its response naturally' : 'Model stopped before finishing'}
			>
				{env.finish_reason}
			</Badge>
		{/if}
		{#if usage}
			<span class="text-muted-foreground">·</span>
			<span class="text-muted-foreground">
				{#if usage.input_tokens !== undefined}
					<span class="font-mono text-foreground">{usage.input_tokens}</span> in
				{/if}
				{#if usage.output_tokens !== undefined}
					<span class="ml-1">+</span>
					<span class="font-mono text-foreground">{usage.output_tokens}</span> out
				{/if}
				{#if usage.total_tokens !== undefined}
					<span class="ml-1 text-sm">({usage.total_tokens} total)</span>
				{/if}
			</span>
		{/if}
	</div>

	{#if responseDefined}
		{#if responseIsString && (env.response as string).length > 0}
			<Markdown content={env.response as string} />
		{:else if responseIsString}
			<div class="text-sm text-muted-foreground italic">Empty response.</div>
		{:else}
			<!-- Structured response — cascade through the registry so flat objects
			     render as KeyValueList, arrays-of-objects as TabularArray, and
			     deeply-nested shapes fall to the CodeMirror JsonBlock instead of
			     a fenced-text dump. Reset nodeKind so we don't re-match this
			     renderer on a nested envelope. -->
			<SmartValue value={env.response} ctx={{ ...ctx, nodeKind: undefined }} />
		{/if}
	{:else}
		<div class="text-sm text-muted-foreground italic">Empty response.</div>
	{/if}

	{#if extras.length > 0}
		<div class="space-y-2">
			{#each extras as [key, val] (key)}
				<div>
					<div class="mb-1 font-mono text-sm text-muted-foreground">{key}</div>
					<!-- Spec-declared output fields are typically the same data as
					     `response` re-keyed (see backend.rs:215-219). Cascade so
					     structured values get the proper renderer instead of being
					     squished into a single-line code chip. -->
					<SmartValue value={val} ctx={{ ...ctx, nodeKind: undefined }} />
				</div>
			{/each}
		</div>
	{/if}
</div>
