<script lang="ts">
	import type { TriggerNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import { FormField } from '$lib/components/ui/form-field';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Button } from '$lib/components/ui/button';
	import * as Select from '$lib/components/ui/select';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import CronPreview from './CronPreview.svelte';
	import TriggerHistory from './TriggerHistory.svelte';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import { browser } from '$app/environment';
	import { onMount } from 'svelte';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	type FieldMapping = components['schemas']['FieldMapping'];
	type PortField = components['schemas']['PortField'];

	type Props = {
		data: TriggerNodeData;
		readonly?: boolean;
		onchange: (data: TriggerNodeData) => void;
		nodeId?: string;
		binding?: YjsGraphBinding;
	};

	let { data, readonly = false, onchange, nodeId, binding }: Props = $props();

	const source = $derived(data.source);
	const sourceKind = $derived(source?.kind ?? 'manual');
	const mappings = $derived(data.payloadMapping ?? []);
	const enabled = $derived(data.enabled ?? false);

	type ReplyMode = components['schemas']['ReplyMode'];
	const replyDefault = $derived((data.replyDefault ?? 'fire_and_forget') as ReplyMode);
	const replyLabels: Record<ReplyMode, string> = {
		fire_and_forget: 'Fire & forget (default)',
		wait_for_result: 'Wait for result',
		sse: 'SSE stream'
	};

	// A trigger's *armed* state is the inverse of every other field here: it is
	// operational state of the published template, not a draft setting. In a
	// draft this is read-only (it ships armed by default); on the published
	// template it is the one live control and writes through the API, not Yjs
	// (the editor binding is frozen for published templates).
	let liveEnabled = $state<boolean | null>(null);
	let toggling = $state(false);
	let toggleError = $state<string | null>(null);

	const displayEnabled = $derived(readonly ? (liveEnabled ?? enabled) : enabled);

	async function refreshLiveEnabled() {
		if (!readonly || !nodeId) return;
		try {
			const res = await fetch('/api/triggers');
			if (!res.ok) return;
			const body = await res.json();
			const t = (body.triggers ?? []).find(
				(x: { node_id: string }) => x.node_id === nodeId
			);
			if (t) liveEnabled = t.enabled;
		} catch {
			// Leave liveEnabled null → fall back to the graph value.
		}
	}

	async function toggleEnabled(next: boolean) {
		// Draft: not an editable setting (inverse of normal freeze).
		if (!readonly || !nodeId) return;
		toggling = true;
		toggleError = null;
		const prev = liveEnabled ?? enabled;
		liveEnabled = next; // optimistic
		try {
			const res = await fetch(`/api/triggers/${encodeURIComponent(nodeId)}/enabled`, {
				method: 'PATCH',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ enabled: next })
			});
			if (!res.ok) {
				liveEnabled = prev;
				toggleError = `Failed to ${next ? 'enable' : 'disable'} (${res.status})`;
				return;
			}
			const body = await res.json();
			if (typeof body.enabled === 'boolean') liveEnabled = body.enabled;
		} catch (e) {
			liveEnabled = prev;
			toggleError = String(e);
		} finally {
			toggling = false;
		}
	}

	onMount(() => {
		void refreshLiveEnabled();
	});

	// Sample-request scaffolding for the API-call (manual) source. The body a
	// caller should POST mirrors the target Start's `initial` schema — but only
	// when there's no payload mapping (mapping rewrites the body, so we warn).
	function sampleValue(f: PortField): unknown {
		switch (f.kind) {
			case 'number':
				return 0;
			case 'bool':
				return false;
			case 'json':
				return {};
			case 'timestamp':
				return '2026-01-01T00:00:00Z';
			case 'select':
				return f.options?.[0]?.value ?? 'option';
			case 'file':
				return 'file-reference';
			case 'signature':
				return 'signature-data';
			case 'textarea':
				return 'example text';
			default:
				return 'example';
		}
	}

	// Rhai keywords/literals that lex as identifiers but aren't scope reads.
	const RHAI_RESERVED = new Set([
		'true',
		'false',
		'null',
		'let',
		'const',
		'if',
		'else',
		'for',
		'in',
		'while',
		'loop',
		'fn',
		'return',
		'switch',
		'this',
		'break',
		'continue',
		'throw',
		'try',
		'catch'
	]);

	// Root scope identifiers a mapping expression reads. At fire time the POSTed
	// `payload`'s top-level keys bind as bare Rhai variables, so the root of each
	// reference chain (before the first `.`/`[`) is a body key the caller must
	// send. Heuristic — covers the bare-ident / member-access shapes mappings
	// use, not a full Rhai parser.
	function rootRefs(expr: string): string[] {
		// Blank out string/char literals so identifiers inside them don't count.
		const stripped = expr.replace(/"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'/g, '""');
		const out: string[] = [];
		const re = /[A-Za-z_$][A-Za-z0-9_$]*/g;
		let m: RegExpExecArray | null;
		while ((m = re.exec(stripped))) {
			const id = m[0];
			const before = stripped.slice(0, m.index).trimEnd();
			const after = stripped.slice(re.lastIndex).trimStart();
			if (before.endsWith('.')) continue; // property access, not a scope var
			if (after.startsWith('(')) continue; // function call name
			if (after.startsWith(':') && !after.startsWith('::')) continue; // map key
			if (RHAI_RESERVED.has(id)) continue;
			out.push(id);
		}
		return out;
	}

	const targetStartFields = $derived.by((): PortField[] => {
		if (!binding || !nodeId) return [];
		const g = binding.graph;
		const edge = g.edges.find((e) => e.source === nodeId);
		if (!edge) return [];
		const tgt = g.nodes.find((n) => n.id === edge.target);
		if (!tgt || tgt.data.type !== 'start') return [];
		return tgt.data.initial?.fields ?? [];
	});

	const fileFields = $derived(targetStartFields.filter((f) => f.kind === 'file'));
	const nonFileFields = $derived(targetStartFields.filter((f) => f.kind !== 'file'));

	// The fire endpoint's JSON contract is `{ "payload": { ...scope keys } }`
	// (FireTriggerRequest.payload) — not the bare object. File fields can't go
	// in JSON; they're uploaded as multipart parts and the server injects a
	// reference object, so they're excluded from the JSON `payload`.
	// With a payload mapping the body the caller must POST is keyed by the
	// identifiers the expressions read (the mapping's input) — not the target
	// Start fields (its output). Without a mapping the trigger forwards
	// `payload` verbatim, so mirror the Start schema.
	const mappedInputKeys = $derived.by(() => {
		const keys = new Set<string>();
		for (const m of mappings) for (const id of rootRefs(m.expression)) keys.add(id);
		return [...keys];
	});
	const samplePayload = $derived.by(() =>
		mappings.length > 0
			? Object.fromEntries(
						mappedInputKeys
							.filter((k) => !fileFields.some((f) => f.name === k))
							.map((k) => [k, 'example'])
					)
			: Object.fromEntries(nonFileFields.map((f) => [f.name, sampleValue(f)]))
	);

	function fileMime(f: PortField): string {
		const first = (f.accept ?? '').split(',')[0]?.trim();
		return first && first.includes('/') ? first : 'application/octet-stream';
	}

	const origin = $derived(browser ? window.location.origin : 'https://YOUR_HOST');
	const fireUrl = $derived(`${origin}/api/triggers/${nodeId ?? '{node_id}'}/fire`);

	// With file fields, fire as multipart/form-data: one `-F` per file part +
	// a `payload` part for the rest. Otherwise a plain JSON body. SSE default
	// adds `-N` + `Accept: text/event-stream` so curl prints events as they
	// arrive on the same response — no second request needed.
	const curlCommand = $derived.by(() => {
		const sse = replyDefault === 'sse';
		const flags = sse ? ' -N' : '';
		const acceptHeader = sse ? [`  -H 'Accept: text/event-stream'`] : [];
		if (fileFields.length > 0) {
			const lines = [`curl${flags} -X POST '${fireUrl}'`, ...acceptHeader];
			for (const f of fileFields) {
				lines.push(`  -F '${f.name}=@/path/to/${f.name};type=${fileMime(f)}'`);
			}
			lines.push(`  -F 'payload=${JSON.stringify(samplePayload)};type=application/json'`);
			return lines.join(' \\\n');
		}
		return [
			`curl${flags} -X POST '${fireUrl}'`,
			`  -H 'Content-Type: application/json'`,
			...acceptHeader,
			`  -d '${JSON.stringify({ payload: samplePayload })}'`
		].join(' \\\n');
	});
	const hasMapping = $derived((data.payloadMapping ?? []).length > 0);

	const sourceKindLabels: Record<string, string> = {
		manual: 'API call',
		cron: 'Cron schedule',
		catalog: 'Catalogue event',
		net_completion: 'Workflow completion',
		webhook: 'Webhook'
	};
	const onStatusLabels: Record<string, string> = {
		success: 'Success',
		failure: 'Failure',
		cancelled: 'Cancelled',
		any: 'Any terminal status'
	};

	// Scope identifiers a mapping expression may reference for the selected
	// source kind. The backend is the single source of truth for the four
	// static kinds; `manual`'s scope is its form, derived client-side.
	let scopeVars = $state<{ name: string; kind: string }[]>([]);

	$effect(() => {
		const kind = sourceKind;
		if (kind === 'manual') {
			const form = (source && source.kind === 'manual' ? source.form : []) ?? [];
			scopeVars = form.map((f) => ({ name: f.name, kind: f.kind }));
			return;
		}
		let cancelled = false;
		fetch(`/api/triggers/source-scope?kind=${encodeURIComponent(kind)}`)
			.then((r) => (r.ok ? r.json() : { scope: [] }))
			.then((body) => {
				if (!cancelled) scopeVars = body.scope ?? [];
			})
			.catch(() => {
				if (!cancelled) scopeVars = [];
			});
		return () => {
			cancelled = true;
		};
	});

	function update<K extends keyof TriggerNodeData>(key: K, value: TriggerNodeData[K]) {
		onchange({ ...data, [key]: value });
	}

	function updateSourceKind(kind: TriggerNodeData['source']['kind']) {
		// Reset source-specific fields when the kind changes — each variant carries
		// different config so we can't preserve fields across kinds.
		const next: TriggerNodeData['source'] =
			kind === 'cron'
				? { kind: 'cron', schedule: '0 9 * * MON-FRI', timezone: 'UTC', jitterSecs: 0 }
				: kind === 'catalog'
					? { kind: 'catalog', filters: {}, backfill: false }
					: kind === 'net_completion'
						? {
								kind: 'net_completion',
								sourceTemplateId: '00000000-0000-0000-0000-000000000000',
								on: 'success'
							}
						: kind === 'webhook'
							? { kind: 'webhook', slug: '', auth: { kind: 'none' } }
							: { kind: 'manual', form: [] };
		update('source', next);
	}

	function addMapping() {
		update('payloadMapping', [...mappings, { targetField: '', expression: 'payload' }]);
	}

	// Identity-map every target Start field so a typed Start is publishable
	// without hand-writing rename-only mappings. The expression form depends
	// on where the field lands in the fire-time scope:
	//  - API call (manual): POST-body top-level keys bind as scope vars, so a
	//    bare identifier resolves (bare idents are also not compile-scoped).
	//  - webhook: the body sits under `payload` → `payload.<name>`.
	//  - cron/catalog/net_completion: best-effort bare name; the user edits
	//    against the fixed source scope (listed above) where it differs.
	function autoMapExpression(fieldName: string): string {
		return sourceKind === 'webhook' ? `payload.${fieldName}` : fieldName;
	}

	function autoMapFromStart() {
		update(
			'payloadMapping',
			targetStartFields.map((f) => ({
				targetField: f.name,
				expression: autoMapExpression(f.name)
			}))
		);
	}

	function updateMapping(idx: number, patch: Partial<FieldMapping>) {
		const next = mappings.map((m, i) => (i === idx ? { ...m, ...patch } : m));
		update('payloadMapping', next);
	}

	function removeMapping(idx: number) {
		update(
			'payloadMapping',
			mappings.filter((_, i) => i !== idx)
		);
	}
</script>

<div class="space-y-3">
	<FormField label="Source kind" for="trigger-source-kind">
		<Select.Root
			type="single"
			value={sourceKind}
			onValueChange={(v) => {
				if (v) updateSourceKind(v as TriggerNodeData['source']['kind']);
			}}
			disabled={readonly}
		>
			<Select.Trigger id="trigger-source-kind" class="w-full" disabled={readonly}>
				{sourceKindLabels[sourceKind] ?? 'API call'}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="manual" label="API call" />
				<Select.Item value="cron" label="Cron schedule" />
				<Select.Item value="catalog" label="Catalogue event" />
				<Select.Item value="net_completion" label="Workflow completion" />
				<Select.Item value="webhook" label="Webhook" />
			</Select.Content>
		</Select.Root>
	</FormField>

	<!-- Source-specific config. Phase 5a keeps it minimal — each source kind
	     gets its own editor in 5b–5e. -->
	{#if source?.kind === 'cron'}
		<FormField label="Cron schedule">
			<Input
				type="text"
				value={source.schedule}
				disabled={readonly}
				oninput={(e) =>
					update('source', { ...source, schedule: (e.currentTarget as HTMLInputElement).value })}
			/>
		</FormField>
		<FormField label="Timezone (IANA)">
			<Input
				type="text"
				value={source.timezone ?? 'UTC'}
				disabled={readonly}
				oninput={(e) =>
					update('source', { ...source, timezone: (e.currentTarget as HTMLInputElement).value })}
			/>
		</FormField>
		<CronPreview schedule={source.schedule} timezone={source.timezone ?? 'UTC'} />
	{:else if source?.kind === 'catalog'}
		<div class="space-y-1.5">
			<div class="flex items-center justify-between">
				<span class="text-sm font-medium text-muted-foreground">Filters (eq only)</span>
				{#if !readonly}
					<Button
						variant="ghost"
						size="sm"
						onclick={() => {
							const next = { ...(source.filters ?? {}) };
							const key = `field${Object.keys(next).length + 1}`;
							next[key] = { eq: '' };
							update('source', { ...source, filters: next });
						}}
					>
						<Plus class="size-3.5" />
						Add
					</Button>
				{/if}
			</div>
			{#each Object.entries(source.filters ?? {}) as [field, ops] (field)}
				<div class="flex items-center gap-1.5">
					<Input
						type="text"
						value={field}
						disabled={readonly}
						placeholder="field"
						oninput={(e) => {
							const next = { ...(source.filters ?? {}) };
							delete next[field];
							next[(e.currentTarget as HTMLInputElement).value] = ops;
							update('source', { ...source, filters: next });
						}}
					/>
					<span class="text-sm text-muted-foreground">=</span>
					<Input
						type="text"
						value={ops.eq ?? ''}
						disabled={readonly}
						placeholder="value"
						oninput={(e) => {
							const next = { ...(source.filters ?? {}) };
							next[field] = { eq: (e.currentTarget as HTMLInputElement).value };
							update('source', { ...source, filters: next });
						}}
					/>
					{#if !readonly}
						<Button
							variant="ghost"
							size="sm"
							onclick={() => {
								const next = { ...(source.filters ?? {}) };
								delete next[field];
								update('source', { ...source, filters: next });
							}}
							aria-label="Remove"
						>
							<Trash2 class="size-3.5" />
						</Button>
					{/if}
				</div>
			{/each}
			<label class="flex items-center gap-2 pt-1">
				<Checkbox
					checked={source.backfill ?? false}
					disabled={readonly}
					onCheckedChange={(checked) =>
						update('source', {
							...source,
							backfill: checked === true
						})}
				/>
				<span class="text-sm">Backfill on publish</span>
			</label>
		</div>
	{:else if source?.kind === 'net_completion'}
		<FormField label="Source template id">
			<Input
				type="text"
				value={source.sourceTemplateId}
				disabled={readonly}
				placeholder="00000000-0000-0000-0000-000000000000"
				oninput={(e) =>
					update('source', {
						...source,
						sourceTemplateId: (e.currentTarget as HTMLInputElement).value
					})}
			/>
		</FormField>
		<FormField label="On status">
			<Select.Root
				type="single"
				value={source.on}
				onValueChange={(v) => {
					if (v)
						update('source', {
							...source,
							on: v as 'success' | 'failure' | 'cancelled' | 'any'
						});
				}}
				disabled={readonly}
			>
				<Select.Trigger class="w-full" disabled={readonly}>
					{onStatusLabels[source.on] ?? 'Success'}
				</Select.Trigger>
				<Select.Content>
					<Select.Item value="success" label="Success" />
					<Select.Item value="failure" label="Failure" />
					<Select.Item value="cancelled" label="Cancelled" />
					<Select.Item value="any" label="Any terminal status" />
				</Select.Content>
			</Select.Root>
		</FormField>
	{:else if source?.kind === 'webhook'}
		<FormField label="Slug" for="trigger-slug">
			<Input
				id="trigger-slug"
				type="text"
				value={source.slug}
				disabled={readonly}
				placeholder="my-webhook"
				oninput={(e) =>
					update('source', { ...source, slug: (e.currentTarget as HTMLInputElement).value })}
			/>
		</FormField>
	{:else if source?.kind === 'manual'}
		<div class="space-y-2">
			<p class="text-sm text-muted-foreground">
				Fires when something <code>POST</code>s to this endpoint — a script, a
				cron job, another service. Add an <code>Authorization</code> header if
				your deployment enforces auth.
			</p>
			<div class="space-y-1.5 rounded-md border border-border/60 bg-muted/20 p-2">
				<div class="flex items-center justify-between gap-2">
					<span class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
						Sample request
					</span>
					<CopyButton text={curlCommand} />
				</div>
				<pre class="overflow-x-auto whitespace-pre-wrap break-all font-mono text-sm leading-relaxed text-foreground">{curlCommand}</pre>
			</div>
			{#if fileFields.length > 0}
				<p class="text-sm text-muted-foreground">
					Replace <code>/path/to/…</code> with your file(s). Each is uploaded to
					storage and passed to the Start as a file reference — no need to
					pre-upload.
				</p>
			{/if}
			{#if targetStartFields.length === 0}
				<p class="text-sm text-muted-foreground">
					{#if !binding || !nodeId}
						Wire this trigger into a Start node to auto-fill the request body
						from its schema.
					{:else}
						The target Start declares no fields — an empty object
						<code>{'{}'}</code> is a valid body.
					{/if}
				</p>
			{/if}
			{#if hasMapping}
				<p class="text-sm text-muted-foreground">
					Body keys are the identifiers your mapping expressions read; the
					values shown are placeholders. The expressions project them onto the
					target Start fields.
				</p>
			{/if}
		</div>
	{/if}

	<!-- Default reply mode. Caller can override per-request via
	     ?reply= / Prefer / body; this is only the fallback. -->
	<FormField label="Default reply mode" for="trigger-reply-default">
		<Select.Root
			type="single"
			value={replyDefault}
			onValueChange={(v) => {
				if (v) update('replyDefault', v as ReplyMode);
			}}
			disabled={readonly}
		>
			<Select.Trigger id="trigger-reply-default" class="w-full" disabled={readonly}>
				{replyLabels[replyDefault]}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="fire_and_forget" label="Fire & forget (default)" />
				<Select.Item value="wait_for_result" label="Wait for result" />
				<Select.Item value="sse" label="SSE stream" />
			</Select.Content>
		</Select.Root>
		<p class="mt-1 text-sm text-muted-foreground">
			Used only when a caller doesn't request a mode. <code>SSE</code> streams
			the instance's events on the fire response itself — a leading
			<code>fire</code> event carries the instance id and outcome, followed by
			domain events through to a final <code>result</code>.
		</p>
	</FormField>

	<!-- Payload mapping — each row projects one target-port field. -->
	<div class="space-y-1.5">
		<div class="flex items-center justify-between">
			<span class="text-sm font-medium text-muted-foreground">Payload mapping</span>
			{#if !readonly}
				<Button variant="ghost" size="sm" onclick={addMapping}>
					<Plus class="size-3.5" />
					Add
				</Button>
			{/if}
		</div>
		{#if scopeVars.length > 0}
			<p class="rounded-md bg-muted/30 p-2 text-sm text-muted-foreground">
				<span class="font-medium">In scope:</span>
				{#each scopeVars as v, i (v.name)}<code
						class="text-foreground">{v.name}</code><span class="text-muted-foreground/70"
					> ({v.kind})</span
					>{#if i < scopeVars.length - 1}, {/if}{/each}
			</p>
		{/if}
		{#if mappings.length === 0}
			{#if targetStartFields.length > 0}
				<div class="space-y-1.5 rounded-md border border-dashed border-border/50 p-2">
					<p class="text-sm text-muted-foreground">
						The target Start declares typed fields, so the payload can't be
						forwarded verbatim — each field needs a mapping. Auto-map adds an
						identity mapping per field; rename or edit them afterward.
					</p>
					{#if !readonly}
						<Button
							variant="outline"
							size="sm"
							onclick={autoMapFromStart}
							data-testid="btn-automap"
						>
							<Plus class="size-3.5" />
							Auto-map from Start schema
						</Button>
					{/if}
				</div>
			{:else}
				<p class="rounded-md border border-dashed border-border/50 p-2 text-sm text-muted-foreground">
					No mappings. Without entries the trigger forwards <code>payload</code>
					verbatim — only valid when the target port has no declared fields.
				</p>
			{/if}
		{:else}
			{#each mappings as mapping, i (i)}
				<div class="rounded-md border border-border/60 bg-muted/20 p-2 space-y-1.5">
					<div class="flex items-center gap-2">
						<Input
							type="text"
							value={mapping.targetField}
							disabled={readonly}
							placeholder="target_field"
							oninput={(e) =>
								updateMapping(i, {
									targetField: (e.currentTarget as HTMLInputElement).value
								})}
						/>
						{#if !readonly}
							<Button
								variant="ghost"
								size="sm"
								onclick={() => removeMapping(i)}
								aria-label="Remove"
							>
								<Trash2 class="size-3.5" />
							</Button>
						{/if}
					</div>
					<Textarea
						value={mapping.expression}
						disabled={readonly}
						rows={2}
						placeholder="payload.x"
						oninput={(e) =>
							updateMapping(i, {
								expression: (e.currentTarget as HTMLTextAreaElement).value
							})}
					/>
				</div>
			{/each}
		{/if}
	</div>

	<!-- Enabled. Inverse of every other field: armed state is operational
	     state of the *published* template, not a draft setting. Read-only in
	     draft (ships armed); the one live control once published. -->
	<label class="flex items-center gap-2">
		<Checkbox
			checked={displayEnabled}
			disabled={!readonly || toggling}
			onCheckedChange={(checked) => toggleEnabled(checked === true)}
		/>
		<span class="text-sm">Enabled</span>
		{#if toggling}<span class="text-sm text-muted-foreground">…</span>{/if}
	</label>
	{#if readonly}
		<p class="text-sm text-muted-foreground">
			Arm or pause this trigger on the published template — takes effect immediately,
			no new version required.
		</p>
	{:else}
		<p class="text-sm text-muted-foreground">
			Triggers ship enabled. Arming and pausing is done on the published template,
			not here — it isn’t a draft setting.
		</p>
	{/if}
	{#if toggleError}
		<p class="text-sm text-destructive">{toggleError}</p>
	{/if}

	{#if nodeId}
		<TriggerHistory {nodeId} />
	{/if}
</div>
