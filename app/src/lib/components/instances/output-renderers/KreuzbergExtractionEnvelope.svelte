<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import KeyValueList from './KeyValueList.svelte';
	import Markdown from './Markdown.svelte';
	import type { RendererProps } from './types';

	// Kreuzberg `ExtractionResult` shape, plus the derived fields the executor
	// stamps for declared-output plumbing (`full_text`, `char_count`,
	// `word_count`, `table_count`). See
	// `executor/crates/executor-kreuzberg/src/backend.rs::build_single_outputs`.
	type Table = {
		markdown?: string;
		page_number?: number | string;
		rows?: number | string;
		[k: string]: unknown;
	};

	type Metadata = {
		language?: string;
		output_format?: string;
		[k: string]: unknown;
	};

	type Extraction = {
		content?: string;
		full_text?: string;
		mime_type?: string;
		metadata?: Metadata;
		tables?: Table[];
		detected_languages?: string[] | null;
		char_count?: number;
		word_count?: number;
		table_count?: number;
		[k: string]: unknown;
	};

	let { value, ctx }: RendererProps = $props();
	const env = $derived(value as Extraction);

	// Prefer the kreuzberg-native `content`; fall back to `full_text` when only
	// the declared-output alias is present. They're typically identical when
	// both exist (the executor stamps `full_text` as an alias for `content`).
	const text = $derived<string>(
		(typeof env.content === 'string' && env.content) ||
			(typeof env.full_text === 'string' && env.full_text) ||
			''
	);
	const hasText = $derived(text.length > 0);

	const tables = $derived<Table[]>(Array.isArray(env.tables) ? env.tables : []);
	const hasTables = $derived(tables.length > 0);

	const language = $derived<string | undefined>(
		(Array.isArray(env.detected_languages) && env.detected_languages[0]) ||
			(typeof env.metadata?.language === 'string' ? env.metadata.language : undefined)
	);

	const metadata = $derived(
		env.metadata && typeof env.metadata === 'object' ? env.metadata : null
	);
	const hasMetadata = $derived(!!metadata && Object.keys(metadata).length > 0);

	// Long bodies get a "Show more" toggle so the drawer doesn't become a
	// scroll-mile when an OCR pass returns a multi-page document.
	const TEXT_PREVIEW_CHARS = 1200;
	const needsTextToggle = $derived(text.length > TEXT_PREVIEW_CHARS);
	let textExpanded = $state(false);
	const visibleText = $derived(
		needsTextToggle && !textExpanded ? text.slice(0, TEXT_PREVIEW_CHARS) : text
	);

	let metadataOpen = $state(false);

	function formatNumber(n: number | undefined): string {
		if (n === undefined || n === null) return '—';
		return new Intl.NumberFormat().format(n);
	}
</script>

<div class="space-y-4">
	<!-- Stat strip: at-a-glance facts pulled from the top-level kreuzberg
	     fields. Mirrors AutomatedStepEnvelope's outcome strip. -->
	<div class="flex flex-wrap items-center gap-2 text-sm">
		{#if env.mime_type}
			<Badge variant="outline" class="font-mono">{env.mime_type}</Badge>
		{/if}
		{#if typeof env.word_count === 'number'}
			<span class="text-muted-foreground">{formatNumber(env.word_count)} words</span>
		{/if}
		{#if typeof env.char_count === 'number'}
			<span class="text-muted-foreground">·</span>
			<span class="text-muted-foreground">{formatNumber(env.char_count)} chars</span>
		{/if}
		{#if typeof env.table_count === 'number' && env.table_count > 0}
			<span class="text-muted-foreground">·</span>
			<span class="text-muted-foreground">
				{env.table_count} table{env.table_count === 1 ? '' : 's'}
			</span>
		{/if}
		{#if language}
			<span class="text-muted-foreground">·</span>
			<Badge variant="outline" class="font-mono">{language}</Badge>
		{/if}
	</div>

	{#if hasText}
		<div>
			<div class="mb-1.5 text-sm font-semibold text-foreground">Extracted text</div>
			<pre
				class="max-h-96 overflow-y-auto rounded-md border border-border bg-muted/20 p-3 font-mono text-sm leading-relaxed whitespace-pre-wrap break-words">{visibleText}{#if needsTextToggle && !textExpanded}…{/if}</pre>
			{#if needsTextToggle}
				<button
					type="button"
					class="mt-1 text-sm text-muted-foreground hover:text-foreground"
					onclick={() => (textExpanded = !textExpanded)}
				>
					{textExpanded
						? 'Show less'
						: `Show full text (${formatNumber(text.length)} chars)`}
				</button>
			{/if}
		</div>
	{/if}

	{#if hasTables}
		<div>
			<div class="mb-1.5 text-sm font-semibold text-foreground">
				Tables
				<span class="ml-1 font-normal text-muted-foreground">({tables.length})</span>
			</div>
			<div class="space-y-3">
				{#each tables as table, i (i)}
					<div class="rounded-md border border-border bg-card">
						<div
							class="flex flex-wrap items-center gap-2 border-b border-border bg-muted/20 px-3 py-1.5 text-sm text-muted-foreground"
						>
							<span class="font-mono">#{i + 1}</span>
							{#if table.page_number !== undefined && table.page_number !== null}
								<span>·</span>
								<span>page {table.page_number}</span>
							{/if}
							{#if table.rows !== undefined && table.rows !== null}
								<span>·</span>
								<span>{table.rows} row{table.rows === 1 ? '' : 's'}</span>
							{/if}
						</div>
						<div class="overflow-x-auto p-3">
							{#if typeof table.markdown === 'string' && table.markdown.length > 0}
								<Markdown content={table.markdown} />
							{:else}
								<p class="text-sm italic text-muted-foreground">
									Table has no rendered body.
								</p>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		</div>
	{/if}

	{#if hasMetadata}
		<div>
			<button
				type="button"
				class="flex items-center gap-1 text-sm font-semibold text-foreground hover:text-muted-foreground"
				onclick={() => (metadataOpen = !metadataOpen)}
			>
				{#if metadataOpen}
					<ChevronDown class="size-3.5" />
				{:else}
					<ChevronRight class="size-3.5" />
				{/if}
				Document metadata
				<span class="ml-1 font-normal text-muted-foreground">
					({Object.keys(metadata!).length} field{Object.keys(metadata!).length === 1
						? ''
						: 's'})
				</span>
			</button>
			{#if metadataOpen}
				<div class="mt-2">
					<KeyValueList value={metadata} {ctx} />
				</div>
			{/if}
		</div>
	{/if}
</div>
