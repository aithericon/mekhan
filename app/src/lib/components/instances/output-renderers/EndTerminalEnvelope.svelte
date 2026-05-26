<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import CheckCircle2 from '@lucide/svelte/icons/check-circle-2';
	import XCircle from '@lucide/svelte/icons/x-circle';
	import KeyValueList from './KeyValueList.svelte';
	import SmartValue from './SmartValue.svelte';
	import JsonBlock from './JsonBlock.svelte';
	import type { RendererProps } from './types';

	// Workflow-exit terminal envelope deposited at `workflow_terminals[*]` (End
	// nodes). Built by `lower_end`'s result_shape transition (success:
	// `exit_code: { ok: true, value: <result_mapping> }`) and by `lower_failure`
	// (`exit_code: { ok: false, error }`), riding on top of the process token's
	// `name` / `process_id` / `task_id` / `status` workflow-level fields.
	//
	// We lead with `exit_code.value` (the actual declared workflow result, what
	// downstream consumers and SubWorkflow joins read) and tuck the workflow
	// metadata behind a disclosure.

	type ExitCode =
		| { ok: true; value?: unknown }
		| { ok: false; error?: unknown }
		| Record<string, unknown>;

	type Terminal = {
		exit_code?: ExitCode;
		name?: string;
		process_id?: string;
		task_id?: string;
		status?: string;
		[k: string]: unknown;
	};

	const KNOWN_META_KEYS: ReadonlySet<string> = new Set([
		'exit_code',
		'name',
		'process_id',
		'task_id',
		'status'
	]);

	let { value, ctx }: RendererProps = $props();
	const env = $derived(value as Terminal);

	const exit = $derived<ExitCode | undefined>(
		env.exit_code && typeof env.exit_code === 'object' ? env.exit_code : undefined
	);
	const isOk = $derived(exit !== undefined && (exit as { ok?: unknown }).ok === true);
	const isErr = $derived(exit !== undefined && (exit as { ok?: unknown }).ok === false);

	// On the success arm `lower_end` wraps the user's result mapping under
	// `value`; older / bare envelopes may park the result fields directly on
	// `exit_code`. Surface whichever is present.
	const result = $derived<unknown>(
		isOk
			? 'value' in (exit as Record<string, unknown>)
				? (exit as { value: unknown }).value
				: undefined
			: undefined
	);
	const error = $derived<unknown>(
		isErr ? (exit as { error?: unknown }).error : undefined
	);

	// Anything carried on the terminal token beyond the canonical workflow
	// metadata is rare but possible (process bridge variants, custom forwarders).
	// Preserve it under "Extra" so nothing is silently dropped from the UI.
	const extras = $derived.by<Record<string, unknown>>(() => {
		const out: Record<string, unknown> = {};
		for (const [k, v] of Object.entries(env)) {
			if (!KNOWN_META_KEYS.has(k)) out[k] = v;
		}
		return out;
	});
	const hasExtras = $derived(Object.keys(extras).length > 0);

	const metadata = $derived<Record<string, unknown>>({
		...(env.name !== undefined ? { name: env.name } : {}),
		...(env.status !== undefined ? { status: env.status } : {}),
		...(env.task_id !== undefined ? { task_id: env.task_id } : {}),
		...(env.process_id !== undefined ? { process_id: env.process_id } : {})
	});

	let metadataOpen = $state(false);
</script>

<div class="space-y-3">
	<!-- Outcome strip — success/failure is the headline for a workflow terminal. -->
	<div class="flex flex-wrap items-center gap-2 text-sm">
		{#if isOk}
			<Badge class="bg-green-100 text-green-700">
				<CheckCircle2 class="size-3.5" />
				<span class="ml-1">success</span>
			</Badge>
		{:else if isErr}
			<Badge class="bg-red-100 text-red-700">
				<XCircle class="size-3.5" />
				<span class="ml-1">failure</span>
			</Badge>
		{/if}
		{#if env.status && env.status !== (isOk ? 'success' : isErr ? 'failure' : undefined)}
			<Badge variant="outline" class="font-mono">{env.status}</Badge>
		{/if}
		{#if env.name}
			<span class="text-muted-foreground">·</span>
			<span class="truncate text-muted-foreground" title={env.name}>{env.name}</span>
		{/if}
	</div>

	{#if isOk}
		{#if result === undefined || result === null}
			<div class="text-sm italic text-muted-foreground">
				Bare workflow exit — no result mapping declared.
			</div>
		{:else}
			<!-- Cascade through SmartValue so the declared result shape (typically
			     a KeyValueList of declared output fields) gets the right renderer.
			     Reset nodeKind so it doesn't accidentally re-match this renderer
			     on a nested envelope. -->
			<SmartValue value={result} ctx={{ ...ctx, nodeKind: undefined }} />
		{/if}
	{:else if isErr}
		<div>
			<div class="mb-1.5 text-sm font-semibold text-destructive">Error</div>
			{#if error === undefined || error === null}
				<div class="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm italic text-destructive">
					Failure declared with no error payload.
				</div>
			{:else if typeof error === 'string'}
				<pre class="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm whitespace-pre-wrap break-words text-destructive">{error}</pre>
			{:else}
				<div class="rounded-md border border-destructive/30 bg-destructive/5 p-3">
					<JsonBlock value={error} {ctx} />
				</div>
			{/if}
		</div>
	{:else}
		<!-- No exit_code at all — bare End with no result mapping. The terminal
		     token is just the workflow metadata; surface it directly instead of
		     hiding everything in the disclosure. -->
		<KeyValueList value={metadata} {ctx} />
	{/if}

	{#if hasExtras}
		<div>
			<div class="mb-1.5 text-sm font-semibold text-muted-foreground">Extra</div>
			<KeyValueList value={extras} {ctx} />
		</div>
	{/if}

	{#if (isOk || isErr) && Object.keys(metadata).length > 0}
		<div>
			<button
				type="button"
				class="flex w-full items-center gap-1 text-left text-sm font-semibold text-muted-foreground hover:text-foreground"
				onclick={() => (metadataOpen = !metadataOpen)}
			>
				{#if metadataOpen}
					<ChevronDown class="size-3.5" />
				{:else}
					<ChevronRight class="size-3.5" />
				{/if}
				Workflow metadata
				<span class="ml-1 font-normal">({Object.keys(metadata).length})</span>
			</button>
			{#if metadataOpen}
				<div class="mt-2">
					<KeyValueList value={metadata} {ctx} />
				</div>
			{/if}
		</div>
	{/if}
</div>
