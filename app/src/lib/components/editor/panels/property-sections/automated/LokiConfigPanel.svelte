<script lang="ts">
	// Loki automated-step config panel.
	//
	// Authoring surface (mirrors `service/src/backends/loki.rs` + the shared
	// `LokiConfig` DTO the mekhan compiler validates and the executor-loki
	// backend runs):
	//  - operation select (query_range | query). `query_range` is the default —
	//    it hits `/loki/api/v1/query_range` over a time window (the usual log
	//    stream mode). `query` hits `/loki/api/v1/query` for an instant query
	//    at a single point in time (typically a metric query).
	//  - Resource binding dropdown (workspace `loki` resources). The connection
	//    (base_url + optional bearer token + optional X-Scope-OrgID tenant)
	//    lives on the resource and is overlaid into the resolved config at run
	//    time. Required — the compiler errors on an empty alias.
	//  - query: monospace LogQL textarea. May carry `{{ slug.field }}` refs the
	//    backend Tera-renders at run time; interpolated values are escaped for
	//    the LogQL double-quoted string literal so an upstream value can't break
	//    out of a matcher (the LogQL analog of binding Postgres values via $1).
	//  - time window (query_range only): `since` (relative look-back, e.g. 1h),
	//    `start` / `end` (RFC3339 or unix-ns, ref-capable), `step` (metric
	//    resolution). Instant queries ignore the window.
	//  - limit (max entries, default 1000), direction (backward = newest-first,
	//    the default), timeout_ms (per-request, default 30000).
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

	// Typed reads with defaults matching the executor's LokiConfig serde
	// defaults so partial drafts deserialize correctly when re-saving.
	const resourceAlias = $derived((config.resource_alias as string | undefined) ?? '');
	const operation = $derived((config.operation as string | undefined) ?? 'query_range');
	const query = $derived((config.query as string | undefined) ?? '');
	const since = $derived((config.since as string | undefined) ?? '');
	const start = $derived((config.start as string | undefined) ?? '');
	const end = $derived((config.end as string | undefined) ?? '');
	const step = $derived((config.step as string | undefined) ?? '');
	const limit = $derived(config.limit as number | undefined);
	const direction = $derived((config.direction as string | undefined) ?? 'backward');
	const timeoutMs = $derived(config.timeout_ms as number | undefined);

	const isRange = $derived(operation === 'query_range');

	const operationLabels: Record<string, string> = {
		query_range: 'Range query (logs over a time window)',
		query: 'Instant query (single point in time)'
	};
	const directionLabels: Record<string, string> = {
		backward: 'Backward (newest first)',
		forward: 'Forward (oldest first)'
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

	// Number field with a serde default (limit / timeout_ms): delete the key
	// when cleared so the default applies, otherwise store the parsed integer.
	function patchNumber(key: string, raw: string) {
		const next = { ...config };
		const v = parseInt(raw, 10);
		if (raw.trim() === '' || Number.isNaN(v)) delete next[key];
		else next[key] = v;
		onchange(next);
	}

	// Loki refs insert directly adjacent (no separating space), matching the
	// Postgres/SMTP convention — placeholders land inside a matcher or window.
	function lokiAppend(target: string, snippet: string): string {
		return appendSnippet(target, snippet, '');
	}
</script>

<div class="space-y-1.5">
	<FormField label="Operation" for="loki-operation">
		<Select.Root
			type="single"
			value={operation}
			onValueChange={(v) => {
				if (v) patch({ operation: v });
			}}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} id="loki-operation" data-testid="loki-operation">
				{operationLabels[operation] ?? operation}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="query_range" label={operationLabels.query_range} />
				<Select.Item value="query" label={operationLabels.query} />
			</Select.Content>
		</Select.Root>
	</FormField>
</div>

<ResourcePicker
	resourceType="loki"
	selected={resourceAlias}
	onChange={(v) => patch({ resource_alias: v })}
	label="Loki resource"
	{readonly}
	testId="loki-resource-select"
	typeLabel="Loki"
/>

<div class="space-y-1.5">
	<FormField label="LogQL query" for="loki-query">
		<Textarea
			id="loki-query"
			value={query}
			placeholder={'{job="varlogs", app="{{ start.app }}"} |= "error"'}
			disabled={readonly}
			oninput={(e) => patch({ query: (e.currentTarget as HTMLTextAreaElement).value })}
			class="min-h-[6rem] font-mono text-sm"
			rows={6}
			data-testid="loki-query"
		/>
	</FormField>
	<p class="text-sm italic text-muted-foreground">
		Use <code class="font-mono">{'{{ slug.field }}'}</code> to splice an upstream value into the
		query. Interpolated values are escaped for the LogQL string literal, so an upstream value
		can't break out of a <code class="font-mono">{'{label="…"}'}</code> matcher.
	</p>
	{#if scope.length > 0 && !readonly}
		<InsertRefButton
			{scope}
			disabled={readonly}
			placeholder="Insert ref into query…"
			oninsert={(s) => patch({ query: lokiAppend(query, s) })}
		/>
	{/if}
</div>

<!-- Time window (range queries only) -->
{#if isRange}
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<span class="text-sm font-medium text-muted-foreground">Time window</span>
		<p class="text-sm italic text-muted-foreground">
			Use a relative <code class="font-mono">since</code> (e.g.
			<code class="font-mono">1h</code>, <code class="font-mono">5m</code>) for a rolling look-back,
			or set explicit <code class="font-mono">start</code> / <code class="font-mono">end</code>
			bounds (RFC3339 or unix nanoseconds). Leaving the window empty falls back to Loki's default.
		</p>
		<FormField label="Since (relative look-back)" for="loki-since">
			<Input
				id="loki-since"
				type="text"
				value={since}
				placeholder="1h"
				disabled={readonly}
				oninput={(e) => patchOptionalString('since', (e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
				data-testid="loki-since"
			/>
		</FormField>
		<div class="grid grid-cols-2 gap-2">
			<FormField label="Start" for="loki-start">
				<Input
					id="loki-start"
					type="text"
					value={start}
					placeholder={'2024-01-01T00:00:00Z'}
					disabled={readonly}
					oninput={(e) => patchOptionalString('start', (e.currentTarget as HTMLInputElement).value)}
					class="font-mono"
					data-testid="loki-start"
				/>
			</FormField>
			<FormField label="End" for="loki-end">
				<Input
					id="loki-end"
					type="text"
					value={end}
					placeholder="now"
					disabled={readonly}
					oninput={(e) => patchOptionalString('end', (e.currentTarget as HTMLInputElement).value)}
					class="font-mono"
					data-testid="loki-end"
				/>
			</FormField>
		</div>
		{#if scope.length > 0 && !readonly}
			<InsertRefButton
				{scope}
				disabled={readonly}
				placeholder="Insert ref into start…"
				oninsert={(s) => patchOptionalString('start', lokiAppend(start, s))}
			/>
		{/if}
		<FormField label="Step (metric resolution)" for="loki-step">
			<Input
				id="loki-step"
				type="text"
				value={step}
				placeholder="30s"
				disabled={readonly}
				oninput={(e) => patchOptionalString('step', (e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
				data-testid="loki-step"
			/>
		</FormField>
	</div>
{/if}

<!-- Limit + direction -->
<div class="grid grid-cols-2 gap-2">
	<FormField label="Limit (max entries)" for="loki-limit">
		<Input
			id="loki-limit"
			type="number"
			min={1}
			value={limit ?? ''}
			placeholder="1000"
			disabled={readonly}
			oninput={(e) => patchNumber('limit', (e.currentTarget as HTMLInputElement).value)}
			data-testid="loki-limit"
		/>
	</FormField>
	<FormField label="Direction" for="loki-direction">
		<Select.Root
			type="single"
			value={direction}
			onValueChange={(v) => {
				if (v) patch({ direction: v });
			}}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} id="loki-direction" data-testid="loki-direction">
				{directionLabels[direction] ?? direction}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="backward" label={directionLabels.backward} />
				<Select.Item value="forward" label={directionLabels.forward} />
			</Select.Content>
		</Select.Root>
	</FormField>
</div>

<!-- Timeout -->
<div class="space-y-1.5">
	<FormField label="Request timeout (ms)" for="loki-timeout">
		<Input
			id="loki-timeout"
			type="number"
			min={1}
			value={timeoutMs ?? ''}
			placeholder="30000"
			disabled={readonly}
			oninput={(e) => patchNumber('timeout_ms', (e.currentTarget as HTMLInputElement).value)}
			data-testid="loki-timeout"
		/>
	</FormField>
	<p class="text-sm italic text-muted-foreground">
		Per-request timeout. Capped at the step's overall job timeout.
	</p>
</div>
