<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import KeyValueList from './KeyValueList.svelte';
	import JsonBlock from './JsonBlock.svelte';
	import type { RendererProps } from './types';

	// Process-rooted tokens carry `_instance_id` (stamped by Start during
	// instance creation) plus other process metadata under `_*` keys.
	// When such a token traverses a HumanTask's wire-edge transition, the
	// `build_human_task_injection_logic` merges in the HumanTask's form
	// scaffold (`steps`, `title`, `instructions_mdsvex`, …). We split the
	// view so the business fields stay prominent and the noisy bits
	// (scaffold + metadata) tuck behind disclosures.
	//
	// This renderer covers:
	//   - Start's parked envelope (`_*` + declared business fields).
	//   - The inbound at HumanTask (above + form scaffold).
	//   - Any later step's view of an upstream Start-routed envelope.

	const PROCESS_META_KEYS: ReadonlySet<string> = new Set([
		'_created_at',
		'_created_by',
		'_instance_id',
		'_process_name',
		'_template_id',
		'_template_version'
	]);

	// `t_<id>_request` (HumanTask submit effect) plus
	// `build_human_task_injection_logic` stamp these on the token going into
	// the HumanTask's entry place. They're properties of the HumanTask
	// itself (its form definition), not data the upstream contributed.
	const HT_SCAFFOLD_KEYS: ReadonlySet<string> = new Set([
		'steps',
		'title',
		'task_title',
		'instructions_mdsvex'
	]);

	let { value, ctx }: RendererProps = $props();
	const env = $derived(value as Record<string, unknown>);

	type Partition = {
		business: Record<string, unknown>;
		scaffold: Record<string, unknown>;
		metadata: Record<string, unknown>;
	};

	const partition = $derived.by<Partition>(() => {
		const business: Record<string, unknown> = {};
		const scaffold: Record<string, unknown> = {};
		const metadata: Record<string, unknown> = {};
		for (const [k, v] of Object.entries(env)) {
			if (PROCESS_META_KEYS.has(k) || k.startsWith('_')) {
				metadata[k] = v;
			} else if (HT_SCAFFOLD_KEYS.has(k)) {
				scaffold[k] = v;
			} else {
				business[k] = v;
			}
		}
		return { business, scaffold, metadata };
	});

	const hasBusiness = $derived(Object.keys(partition.business).length > 0);
	const hasScaffold = $derived(Object.keys(partition.scaffold).length > 0);
	const hasMetadata = $derived(Object.keys(partition.metadata).length > 0);

	const processName = $derived<string | null>(
		typeof env._process_name === 'string' ? env._process_name : null
	);
	const instanceId = $derived<string | null>(
		typeof env._instance_id === 'string' ? env._instance_id : null
	);

	let scaffoldOpen = $state(false);
	let metadataOpen = $state(false);
</script>

<div class="space-y-3">
	{#if processName || instanceId}
		<div class="flex flex-wrap items-center gap-2 text-sm">
			{#if processName}
				<Badge variant="outline" class="font-mono">{processName}</Badge>
			{/if}
			{#if instanceId}
				<span class="text-muted-foreground">instance</span>
				<code class="rounded bg-muted px-1.5 py-0.5 font-mono text-sm">{instanceId.slice(0, 8)}…</code>
			{/if}
		</div>
	{/if}

	{#if hasBusiness}
		<KeyValueList value={partition.business} {ctx} />
	{:else}
		<div class="text-sm text-muted-foreground italic">No business fields on this token.</div>
	{/if}

	{#if hasScaffold}
		<div>
			<button
				type="button"
				class="flex w-full items-center gap-1 text-left text-sm font-semibold text-muted-foreground hover:text-foreground"
				onclick={() => (scaffoldOpen = !scaffoldOpen)}
			>
				{#if scaffoldOpen}
					<ChevronDown class="size-3.5" />
				{:else}
					<ChevronRight class="size-3.5" />
				{/if}
				Form definition
				<span class="ml-1 font-normal">({Object.keys(partition.scaffold).length})</span>
			</button>
			{#if scaffoldOpen}
				<div class="mt-2">
					<JsonBlock value={partition.scaffold} {ctx} />
				</div>
			{/if}
		</div>
	{/if}

	{#if hasMetadata}
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
				Process metadata
				<span class="ml-1 font-normal">({Object.keys(partition.metadata).length})</span>
			</button>
			{#if metadataOpen}
				<div class="mt-2">
					<KeyValueList value={partition.metadata} {ctx} />
				</div>
			{/if}
		</div>
	{/if}
</div>
