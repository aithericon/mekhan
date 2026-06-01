<script lang="ts">
	// Postgres automated-step config panel.
	//
	// Authoring surface (mirrors `service/src/backends/postgres.rs` and the
	// shared `PostgresConfig` DTO the mekhan compiler validates):
	//  - operation select (read | write). `read` is the safe default — it runs
	//    the query under SET LOCAL transaction_read_only.
	//  - Resource binding dropdown (workspace `postgres` resources). The
	//    connection (host/port/database/user/password/sslmode) lives on the
	//    resource and is overlaid into the resolved config at run time.
	//  - query: monospace SQL textarea. May carry `$1..` bind params and
	//    `{{ident:slug.field}}` identifier refs (double-quoted at run time).
	//  - params: an ordered list of `$1..` bindings. Each row is free text:
	//    literal JSON (scalar / array / object) OR a whole-placeholder
	//    `{{slug.field}}` ref the backend resolves to a typed bind value.
	//  - projection: declared output columns. Required for `read` (drives the
	//    output schema); optional for `write` (validates RETURNING columns).
	//  - RLS context (optional): a `setting` + `value` injected via
	//    `set_config(<setting>, <value>, true)` (SET LOCAL scope).
	//
	// Persistence follows the repo's onchange-config idiom (NOT bind:) — the
	// panel emits a fresh config object via `onchange`, identical to
	// SmtpConfigPanel.

	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
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

	// Typed projections with defaults matching the executor's PostgresConfig
	// defaults so partial drafts deserialize correctly when re-saving.
	const resourceAlias = $derived((config.resource_alias as string | undefined) ?? '');
	const operation = $derived((config.operation as string | undefined) ?? 'read');
	const query = $derived((config.query as string | undefined) ?? '');
	const params = $derived((config.params as unknown[] | undefined) ?? []);
	const projection = $derived((config.projection as string[] | undefined) ?? []);
	const rowLimit = $derived(config.row_limit as number | undefined);
	const statementTimeoutMs = $derived(config.statement_timeout_ms as number | undefined);
	const rls = $derived(
		(config.rls_context as { setting?: string; value?: string } | undefined) ?? null
	);

	const operationLabels: Record<string, string> = {
		read: 'Read (SELECT, read-only transaction)',
		write: 'Write (INSERT / UPDATE / DELETE)'
	};

	function patch(updates: Record<string, unknown>) {
		onchange({ ...config, ...updates });
	}

	// --- params ---
	// Each param row is stored as a raw string in the wire config. A row is
	// either literal JSON (`42`, `"foo"`, `[1,2,3]`, `{"k":1}`) or a
	// whole-placeholder ref (`{{slug.field}}`). The backend resolves refs and
	// types literals itself — the editor keeps them as opaque text.
	function paramText(p: unknown): string {
		if (typeof p === 'string') return p;
		try {
			return JSON.stringify(p);
		} catch {
			return String(p);
		}
	}

	function setParams(next: unknown[]) {
		patch({ params: next });
	}

	function updateParam(idx: number, value: string) {
		const next = [...params];
		next[idx] = value;
		setParams(next);
	}

	function addParam() {
		setParams([...params, '']);
	}

	function removeParam(idx: number) {
		setParams(params.filter((_, i) => i !== idx));
	}

	// --- projection ---
	function setProjection(next: string[]) {
		patch({ projection: next });
	}

	function updateProjection(idx: number, value: string) {
		const next = [...projection];
		next[idx] = value;
		setProjection(next);
	}

	function addProjectionCol() {
		setProjection([...projection, '']);
	}

	function removeProjectionCol(idx: number) {
		setProjection(projection.filter((_, i) => i !== idx));
	}

	// --- RLS context (optional) ---
	let rlsOpen = $state(false);
	$effect(() => {
		// Auto-expand when an RLS context already exists on the persisted config.
		if (rls !== null) rlsOpen = true;
	});

	function setRlsField(field: 'setting' | 'value', value: string) {
		const next = { ...(rls ?? {}), [field]: value };
		patch({ rls_context: next });
	}

	function clearRls() {
		const { rls_context: _unused, ...rest } = config as Record<string, unknown> & {
			rls_context?: unknown;
		};
		onchange(rest);
		rlsOpen = false;
	}

	// Postgres refs insert directly adjacent (no separating space), matching
	// the SMTP panel's sep='' convention for template/placeholder insertion.
	function pgAppend(target: string, snippet: string): string {
		return appendSnippet(target, snippet, '');
	}
</script>

<div class="space-y-1.5">
	<FormField label="Operation" for="pg-operation">
		<Select.Root
			type="single"
			value={operation}
			onValueChange={(v) => {
				if (v) patch({ operation: v });
			}}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} id="pg-operation" data-testid="pg-operation">
				{operationLabels[operation] ?? operation}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="read" label={operationLabels.read} />
				<Select.Item value="write" label={operationLabels.write} />
			</Select.Content>
		</Select.Root>
	</FormField>
</div>

<ResourcePicker
	resourceType="postgres"
	selected={resourceAlias}
	onChange={(v) => patch({ resource_alias: v })}
	label="Postgres resource"
	{readonly}
	testId="pg-resource-select"
	typeLabel="Postgres"
/>

<div class="space-y-1.5">
	<FormField label="SQL query" for="pg-query">
		<Textarea
			id="pg-query"
			value={query}
			placeholder={'SELECT * FROM {{ident:tbl.name}} WHERE id = $1'}
			disabled={readonly}
			oninput={(e) => patch({ query: (e.currentTarget as HTMLTextAreaElement).value })}
			class="min-h-[6rem] font-mono text-sm"
			rows={6}
			data-testid="pg-query"
		/>
	</FormField>
	<p class="text-sm italic text-muted-foreground">
		Use <code class="font-mono">$1, $2, …</code> for bind params (listed below) and
		<code class="font-mono">{'{{ident:slug.field}}'}</code> for runtime-validated identifiers
		(emitted double-quoted).
	</p>
	{#if scope.length > 0 && !readonly}
		<InsertRefButton
			{scope}
			disabled={readonly}
			placeholder="Insert identifier ref into query…"
			oninsert={(s) => patch({ query: pgAppend(query, s) })}
		/>
	{/if}
</div>

<!-- Params ($1..) -->
<div class="space-y-1">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Parameters ($1, $2, …)</span>
		{#if !readonly}
			<button
				type="button"
				class="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-sm text-muted-foreground hover:bg-accent hover:text-foreground"
				onclick={addParam}
				data-testid="pg-add-param"
			>
				<Plus class="size-3" />
				Add
			</button>
		{/if}
	</div>
	{#if params.length === 0}
		<p class="text-sm italic text-muted-foreground">
			None. Each row is literal JSON (e.g. <code class="font-mono">42</code>,
			<code class="font-mono">"foo"</code>, <code class="font-mono">[1,2,3]</code>) or a whole
			<code class="font-mono">{'{{slug.field}}'}</code> ref.
		</p>
	{:else}
		<div class="space-y-1">
			{#each params as p, idx (idx)}
				<div class="flex items-center gap-1.5">
					<span class="w-7 shrink-0 text-right font-mono text-sm text-muted-foreground"
						>${idx + 1}</span
					>
					<Input
						type="text"
						value={paramText(p)}
						placeholder={'{{ slug.field }} or literal JSON'}
						disabled={readonly}
						oninput={(e) => updateParam(idx, (e.currentTarget as HTMLInputElement).value)}
						class="min-w-0 flex-1 font-mono"
						data-testid={`pg-param-${idx}`}
					/>
					{#if scope.length > 0 && !readonly}
						<div class="w-28 shrink-0">
							<InsertRefButton
								{scope}
								disabled={readonly}
								placeholder="Insert ref…"
								oninsert={(s) => updateParam(idx, pgAppend(paramText(p), s))}
							/>
						</div>
					{/if}
					{#if !readonly}
						<button
							type="button"
							class="rounded p-1 text-muted-foreground hover:text-destructive"
							onclick={() => removeParam(idx)}
							title="Remove parameter"
						>
							<Trash2 class="size-3.5" />
						</button>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</div>

<!-- Projection (output columns) -->
<div class="space-y-1">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">
			Projection {operation === 'read' ? '(required)' : '(optional — validates RETURNING)'}
		</span>
		{#if !readonly}
			<button
				type="button"
				class="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-sm text-muted-foreground hover:bg-accent hover:text-foreground"
				onclick={addProjectionCol}
				data-testid="pg-add-projection"
			>
				<Plus class="size-3" />
				Add
			</button>
		{/if}
	</div>
	{#if projection.length === 0}
		<p class="text-sm italic text-muted-foreground">
			{operation === 'read'
				? 'At least one column required — declares the output `rows` schema.'
				: 'None. Add columns to validate a RETURNING clause.'}
		</p>
	{:else}
		<div class="space-y-1">
			{#each projection as col, idx (idx)}
				<div class="flex items-center gap-1.5">
					<Input
						type="text"
						value={col}
						placeholder="column_name"
						disabled={readonly}
						oninput={(e) => updateProjection(idx, (e.currentTarget as HTMLInputElement).value)}
						class="min-w-0 flex-1 font-mono"
						data-testid={`pg-projection-${idx}`}
					/>
					{#if !readonly}
						<button
							type="button"
							class="rounded p-1 text-muted-foreground hover:text-destructive"
							onclick={() => removeProjectionCol(idx)}
							title="Remove column"
						>
							<Trash2 class="size-3.5" />
						</button>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</div>

<!-- Limits -->
<div class="grid grid-cols-2 gap-2">
	<FormField label="Row limit" for="pg-row-limit">
		<Input
			id="pg-row-limit"
			type="number"
			min={1}
			value={rowLimit ?? ''}
			placeholder="10000"
			disabled={readonly}
			oninput={(e) => {
				const val = parseInt((e.currentTarget as HTMLInputElement).value, 10);
				patch({ row_limit: Number.isNaN(val) ? undefined : val });
			}}
		/>
	</FormField>
	<FormField label="Statement timeout (ms)" for="pg-stmt-timeout">
		<Input
			id="pg-stmt-timeout"
			type="number"
			min={1}
			value={statementTimeoutMs ?? ''}
			placeholder="5000"
			disabled={readonly}
			oninput={(e) => {
				const val = parseInt((e.currentTarget as HTMLInputElement).value, 10);
				patch({ statement_timeout_ms: Number.isNaN(val) ? undefined : val });
			}}
		/>
	</FormField>
</div>

<!-- RLS context (optional) -->
<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
	<button
		type="button"
		class="flex w-full items-center justify-between text-sm font-medium text-muted-foreground hover:text-foreground"
		onclick={() => (rlsOpen = !rlsOpen)}
		data-testid="pg-rls-toggle"
	>
		<span class="flex items-center gap-1">
			{#if rlsOpen}
				<ChevronDown class="size-3.5" />
			{:else}
				<ChevronRight class="size-3.5" />
			{/if}
			RLS context (optional)
		</span>
		{#if rls !== null}
			<span class="font-mono text-sm text-emerald-600">set</span>
		{/if}
	</button>
	{#if rlsOpen}
		<p class="text-sm italic text-muted-foreground">
			Injected as <code class="font-mono">set_config(setting, value, true)</code> (SET LOCAL scope)
			before the query runs.
		</p>
		<FormField label="Setting (identifier)" for="pg-rls-setting">
			<Input
				id="pg-rls-setting"
				type="text"
				value={rls?.setting ?? ''}
				placeholder="app.current_tenant"
				disabled={readonly}
				oninput={(e) => setRlsField('setting', (e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
				data-testid="pg-rls-setting"
			/>
		</FormField>
		<div class="space-y-1.5">
			<FormField label="Value" for="pg-rls-value">
				<Input
					id="pg-rls-value"
					type="text"
					value={rls?.value ?? ''}
					placeholder={'literal or {{ slug.field }}'}
					disabled={readonly}
					oninput={(e) => setRlsField('value', (e.currentTarget as HTMLInputElement).value)}
					class="font-mono"
					data-testid="pg-rls-value"
				/>
			</FormField>
			{#if scope.length > 0 && !readonly}
				<InsertRefButton
					{scope}
					disabled={readonly}
					placeholder="Insert ref into value…"
					oninsert={(s) => setRlsField('value', pgAppend(rls?.value ?? '', s))}
				/>
			{/if}
		</div>
		{#if !readonly && rls !== null}
			<button
				type="button"
				class="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-sm text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
				onclick={clearRls}
			>
				<Trash2 class="size-3" />
				Remove RLS context
			</button>
		{/if}
	{/if}
</div>
