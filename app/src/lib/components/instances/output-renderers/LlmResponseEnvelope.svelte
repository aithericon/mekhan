<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import Markdown from './Markdown.svelte';
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

	let { value }: RendererProps = $props();
	const env = $derived(value as Envelope);

	const responseText = $derived(
		typeof env.response === 'string' ? env.response : JSON.stringify(env.response, null, 2)
	);

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

	{#if responseText}
		<div class="rounded-md border border-border bg-muted/20 p-4">
			<Markdown content={responseText} />
		</div>
	{:else}
		<div class="text-sm text-muted-foreground italic">Empty response.</div>
	{/if}

	{#if extras.length > 0}
		<dl class="grid grid-cols-[minmax(8rem,max-content)_1fr] gap-x-4 gap-y-2 text-sm">
			{#each extras as [key, val] (key)}
				<dt class="font-mono text-muted-foreground">{key}</dt>
				<dd class="break-words">
					{#if typeof val === 'string'}
						<span>{val}</span>
					{:else}
						<code class="rounded bg-muted px-1.5 py-0.5 font-mono text-sm">{JSON.stringify(val)}</code>
					{/if}
				</dd>
			{/each}
		</dl>
	{/if}
</div>
