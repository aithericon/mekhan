<script lang="ts">
	// Prometheus automated-step config panel.
	//
	// Authoring surface (mirrors `service/src/backends/prometheus.rs` + the
	// shared `PrometheusConfig` DTO the mekhan compiler validates and the
	// executor-prometheus backend runs):
	//  - operation select (query | query_range). `query` is the default — it
	//    hits `/api/v1/query` for an instant query (the current value of an
	//    expression at a single point in time). `query_range` hits
	//    `/api/v1/query_range` to evaluate an expression over a time window at
	//    a fixed resolution step.
	//  - Resource binding dropdown (workspace `prometheus` resources). The
	//    connection (base_url + optional bearer token + optional X-Scope-OrgID
	//    tenant) lives on the resource and is overlaid into the resolved config
	//    at run time. Required — the compiler errors on an empty alias.
	//  - query: monospace PromQL textarea. May carry `{{ slug.field }}` refs the
	//    backend Tera-renders at run time; interpolated values are escaped for
	//    the PromQL double-quoted string literal so an upstream value can't break
	//    out of a label matcher (the PromQL analog of binding Postgres values
	//    via $1).
	//  - time (instant only): optional evaluation timestamp (RFC3339 or unix
	//    seconds, ref-capable). Omitted = evaluate at "now".
	//  - time window (query_range only): `since` (relative look-back, e.g. 5m),
	//    `start` / `end` (RFC3339 or unix seconds, ref-capable), `step`
	//    (required resolution, default 15s).
	//  - timeout_ms (per-request, default 30000).
	//
	// Persistence follows the repo's onchange-config idiom (NOT bind:) — the
	// panel emits a fresh config object via `onchange`, identical to the
	// Postgres / SMTP panels. Optional string fields delete their key when
	// cleared so they serialize as `None`, not `Some("")`.

	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import InsertRefButton from '../InsertRefButton.svelte';
	import ResourcePicker from '../shared/ResourcePicker.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { appendSnippet } from '$lib/editor/append-snippet';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
		scope?: ScopeEntry[];
	};

	let { config, readonly = false, onchange, scope = [] }: Props = $props();

	// Typed reads with defaults matching the executor's PrometheusConfig serde
	// defaults so partial drafts deserialize correctly when re-saving.
	const resourceAlias = $derived((config.resource_alias as string | undefined) ?? '');
	const operation = $derived((config.operation as string | undefined) ?? 'query');
	const query = $derived((config.query as string | undefined) ?? '');
	const time = $derived((config.time as string | undefined) ?? '');
	const since = $derived((config.since as string | undefined) ?? '');
	const start = $derived((config.start as string | undefined) ?? '');
	const end = $derived((config.end as string | undefined) ?? '');
	const step = $derived((config.step as string | undefined) ?? '');
	const timeoutMs = $derived(config.timeout_ms as number | undefined);

	const isRange = $derived(operation === 'query_range');

	const operationLabels: Record<string, string> = {
		query: 'Instant query (current value)',
		query_range: 'Range query (over a window)'
	};

	function patch(updates: Record<string, unknown>) {
		onchange({ ...config, ...updates });
	}

	// Optional string field: set when non-empty, delete the key when cleared so
	// the wire config omits it (serde `Option::is_none`) rather than carrying an
	// empty string the backend would treat as a present-but-blank value.
	function patchOptionalString(key: string, value: string) {
		const next = { ...config };
		if (value.trim() === '') delete next[key];
		else next[key] = value;
		onchange(next);
	}

	// Number field with a serde default (timeout_ms): delete the key when
	// cleared so the default applies, otherwise store the parsed integer.
	function patchNumber(key: string, raw: string) {
		const next = { ...config };
		const v = parseInt(raw, 10);
		if (raw.trim() === '' || Number.isNaN(v)) delete next[key];
		else next[key] = v;
		onchange(next);
	}

	// Prometheus refs insert directly adjacent (no separating space), matching
	// the Postgres/SMTP convention — placeholders land inside a matcher or
	// window.
	function promAppend(target: string, snippet: string): string {
		return appendSnippet(target, snippet, '');
	}
</script>

<div class="space-y-1.5">
	<FormField label="Operation" for="prometheus-operation">
		<Select.Root
			type="single"
			value={operation}
			onValueChange={(v) => {
				if (v) patch({ operation: v });
			}}
			disabled={readonly}
		>
			<Select.Trigger
				disabled={readonly}
				id="prometheus-operation"
				data-testid="prometheus-operation"
			>
				{operationLabels[operation] ?? operation}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="query" label={operationLabels.query} />
				<Select.Item value="query_range" label={operationLabels.query_range} />
			</Select.Content>
		</Select.Root>
	</FormField>
</div>

<ResourcePicker
	resourceType="prometheus"
	selected={resourceAlias}
	onChange={(v) => patch({ resource_alias: v })}
	label="Prometheus resource"
	{readonly}
	testId="prom-resource-select"
	typeLabel="Prometheus"
/>

<div class="space-y-1.5">
	<FormField label="PromQL query" for="prometheus-query">
		<Textarea
			id="prometheus-query"
			value={query}
			placeholder={'up{job="{{ start.job }}"}'}
			disabled={readonly}
			oninput={(e) => patch({ query: (e.currentTarget as HTMLTextAreaElement).value })}
			class="min-h-[6rem] font-mono text-sm"
			rows={6}
			data-testid="prometheus-query"
		/>
	</FormField>
	<p class="text-sm italic text-muted-foreground">
		Use <code class="font-mono">{'{{ slug.field }}'}</code> to splice an upstream value into the
		query. Interpolated values are escaped for the PromQL string literal, so an upstream value
		can't break out of a <code class="font-mono">{'{label="…"}'}</code> matcher.
	</p>
	{#if scope.length > 0 && !readonly}
		<InsertRefButton
			{scope}
			disabled={readonly}
			placeholder="Insert ref into query…"
			oninsert={(s) => patch({ query: promAppend(query, s) })}
		/>
	{/if}
</div>

<!-- Evaluation time (instant queries only) -->
{#if !isRange}
	<div class="space-y-1.5">
		<FormField label="Time (evaluation timestamp)" for="prometheus-time">
			<Input
				id="prometheus-time"
				type="text"
				value={time}
				placeholder={'2024-01-01T00:00:00Z'}
				disabled={readonly}
				oninput={(e) => patchOptionalString('time', (e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
				data-testid="prometheus-time"
			/>
		</FormField>
		<p class="text-sm italic text-muted-foreground">
			Optional evaluation point (RFC3339 or unix seconds). Leave empty to evaluate at the current
			time.
		</p>
		{#if scope.length > 0 && !readonly}
			<InsertRefButton
				{scope}
				disabled={readonly}
				placeholder="Insert ref into time…"
				oninsert={(s) => patchOptionalString('time', promAppend(time, s))}
			/>
		{/if}
	</div>
{/if}

<!-- Time window (range queries only) -->
{#if isRange}
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<span class="text-sm font-medium text-muted-foreground">Time window</span>
		<p class="text-sm italic text-muted-foreground">
			Use a relative <code class="font-mono">since</code> (e.g.
			<code class="font-mono">1h</code>, <code class="font-mono">5m</code>) for a rolling look-back,
			or set explicit <code class="font-mono">start</code> / <code class="font-mono">end</code>
			bounds (RFC3339 or unix seconds). <code class="font-mono">step</code> is required.
		</p>
		<FormField label="Since (relative look-back)" for="prometheus-since">
			<Input
				id="prometheus-since"
				type="text"
				value={since}
				placeholder="5m"
				disabled={readonly}
				oninput={(e) => patchOptionalString('since', (e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
				data-testid="prometheus-since"
			/>
		</FormField>
		<div class="grid grid-cols-2 gap-2">
			<FormField label="Start" for="prometheus-start">
				<Input
					id="prometheus-start"
					type="text"
					value={start}
					placeholder={'2024-01-01T00:00:00Z'}
					disabled={readonly}
					oninput={(e) => patchOptionalString('start', (e.currentTarget as HTMLInputElement).value)}
					class="font-mono"
					data-testid="prometheus-start"
				/>
			</FormField>
			<FormField label="End" for="prometheus-end">
				<Input
					id="prometheus-end"
					type="text"
					value={end}
					placeholder="now"
					disabled={readonly}
					oninput={(e) => patchOptionalString('end', (e.currentTarget as HTMLInputElement).value)}
					class="font-mono"
					data-testid="prometheus-end"
				/>
			</FormField>
		</div>
		{#if scope.length > 0 && !readonly}
			<InsertRefButton
				{scope}
				disabled={readonly}
				placeholder="Insert ref into start…"
				oninsert={(s) => patchOptionalString('start', promAppend(start, s))}
			/>
		{/if}
		<FormField label="Step (resolution)" for="prometheus-step" required>
			<Input
				id="prometheus-step"
				type="text"
				value={step || '15s'}
				placeholder="15s"
				disabled={readonly}
				oninput={(e) => patchOptionalString('step', (e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
				data-testid="prometheus-step"
			/>
		</FormField>
	</div>
{/if}

<!-- Timeout -->
<div class="space-y-1.5">
	<FormField label="Request timeout (ms)" for="prometheus-timeout">
		<Input
			id="prometheus-timeout"
			type="number"
			min={1}
			value={timeoutMs ?? ''}
			placeholder="30000"
			disabled={readonly}
			oninput={(e) => patchNumber('timeout_ms', (e.currentTarget as HTMLInputElement).value)}
			data-testid="prometheus-timeout"
		/>
	</FormField>
	<p class="text-sm italic text-muted-foreground">
		Per-request timeout. Capped at the step's overall job timeout.
	</p>
</div>
